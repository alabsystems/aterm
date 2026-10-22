// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! The pet types beside its guess.
//!
//! While the person types, the native host paints each letter before the program
//! echoes it (predictive echo) and reserves the guessed cells from the pet: the
//! caret's own cell, one cell left of the escort's seat. A body whose pose widens
//! (waking, a landing squash) overhangs that cell by a sliver. Reserved as a
//! PROTECTED surface, the guess made the pet's step-aside throw the body up onto a
//! border rule, down under the composer or three columns ahead in one frame, and the
//! echo a few milliseconds later turned the cell back into ink and the chase pulled
//! it home: on every keystroke. That was the owner's "jumping and jittering" cat
//! (2026-10-20, Claude Code generating while a long prompt was typed).
//!
//! The guess is the person's own ink, early, and is reserved as ink
//! (`PetReserve::ink`). These laws replay Claude Code's recorded bytes through a real
//! `Terminal` and the shipping `CompanionOwner`, presented every 8 ms (a 120 Hz
//! display) under the native host's frame hold, with the guess the host's predictor
//! reports (`pending_bounds`: each typed letter at the caret, until the echo moves
//! the cursor past it). [`Reserve::Protected`] reserves the same cells as the host
//! did before: the negative control.
//!
//! The same replay host measures the legs (the gait art drawn while typing,
//! at 120 and 60 Hz and with a slow echo), the composer growing a line under a
//! cat beside the host's footer chips (the chips reserved as the host reserves
//! them), and the bound across the composer's wrap.

use std::time::{Duration, Instant};

use aterm_core::render::RenderInput;
use aterm_core::terminal::{ClockReading, Terminal};
use aterm_effects::companion::{CompanionOwner, GlowOwnership, PetFacts};
use aterm_effects::cursor_glow::GlowStyle;
use aterm_effects::host::{
    CaptureMode, FrameGeom, HostFrameInput, SingFacts, TerminalFacts, Visibility,
};
use aterm_effects::kitty_pet::{PetAction, PetFrame, PetInputKind};
use aterm_effects::pet_world::{PetPane, PetRect, PetReserve, PetWorldFacts};
use aterm_effects::word_decorations::{DecoConfig, WordDecorations};
use aterm_lexicon::Lexicon;

use crate::common::{FrameHold, Rec, recording};

/// The owner's cell (15x28 px, a Retina 2x).
const CW: u16 = 15;
const CH: u16 = 28;
/// The pet's top chase speed (`kitty_pet::MAX_SPEED`, columns per second).
const MAX_SPEED: f32 = 26.0;
/// A gait's art drawn again this soon after the cat left it is a flicker.
const FLICKER_MS: u64 = 350;
/// An unconfirmed guess expires (`aterm_predict`'s `GLITCH_FLOOR_MS`, the
/// window's floor on a local link).
const GUESS_EXPIRY_MS: u64 = 250;

struct Take {
    name: &'static str,
    src: &'static str,
    rows: u16,
    cols: u16,
}

/// Claude Code typing takes. The owner's case first: Claude Code 2.1.304 generating
/// (its spinner repainting every ~100 ms) while a 220-character prompt is typed at
/// ~9 keys/s and wraps at the right edge, then typing after the turn (40x120,
/// recorded 2026-10-20 in the jailed real-Claude lab against its stub API, each key
/// stamped when aterm had written it). Then a line typed at 12 cps (2.1.278,
/// 40x120), a line typed across the composer's wrap (2.1.280, 40x120), and the
/// owner's 175-key mid-line insert (60x146).
const TAKES: [Take; 4] = [
    Take {
        name: "generating",
        src: include_str!("fixtures/claude-generating-typing-2026-10-20.ptylog"),
        rows: 40,
        cols: 120,
    },
    Take {
        name: "composer",
        src: include_str!("fixtures/claude-composer-2026-09-21.ptylog"),
        rows: 40,
        cols: 120,
    },
    Take {
        name: "wrap-end",
        src: include_str!("fixtures/claude-wrap-2026-09-23-end.ptylog"),
        rows: 40,
        cols: 120,
    },
    Take {
        name: "long-insert",
        src: include_str!("fixtures/claude-long-insert-2026-10-17.ptylog"),
        rows: 60,
        cols: 146,
    },
];

/// How the host reserves the guessed cells from the pet.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum Reserve {
    /// As it ships: the person's ink.
    Ink,
    /// As it did until 2026-10-20: a protected surface.
    Protected,
}

/// One replay's host: how it reserves the guess, its display, the program's
/// echo latency, and whether aterm's footer chips sit on the composer's rule.
#[derive(Clone, Copy, Debug)]
struct Knobs {
    reserve: Reserve,
    /// The display's frame interval: 8 ms is 120 Hz, 16 ms 60 Hz.
    frame_ms: u64,
    /// The program's output arrives this much later than recorded (a slow
    /// echo, a remote link).
    echo_ms: u64,
    /// The host's footer chips (`claude_footer`: model, folder, branch, cost)
    /// spliced right-aligned into the composer's bottom rule and reserved as
    /// protected chrome, as `pet_chrome` reserves them: the last
    /// [`CHIPS_COLS`] cells before the rule's last.
    chips: bool,
    /// The harness on: its tint over the composer's top rule, the CLI's
    /// lockup `    claude ` (the icon's sprite over its two blank cells, and
    /// the name) written at its left and the lit switch ` alab harness ●
    /// on ` at its right (`claude_footer::compose_top` repaints the rule's
    /// whole run). `pet_chrome` reserves the lockup and the switch as
    /// protected chrome and leaves the tinted strokes the rule they are
    /// (`PetChrome::note_painted`). The composer's first line sits under
    /// that rule, and a 1.7-row body standing on it reaches into it.
    brand: bool,
    /// The composer's upper rule reserved from end to end, as the host
    /// reserved its tint until 2026-10-20 (`PetChrome::note_painted`): no
    /// stand on the composer's first line, so the cat rests under the
    /// composer while it is typed, beside the chips.
    rule_walled: bool,
}

impl Knobs {
    /// The owner's host: the guess as ink, 120 Hz, the recorded echo, no chips.
    const SHIPPED: Self = Self {
        reserve: Reserve::Ink,
        frame_ms: 8,
        echo_ms: 0,
        chips: false,
        brand: false,
        rule_walled: false,
    };
}

/// The footer chips' width: `◆ Opus 5.5 xhigh ⌂ ~/aterm ⎇ main Σ $1.23` and
/// their gaps, as the owner's footer writes them at 120 columns.
const CHIPS_COLS: u16 = 57;

/// The CLI's lockup on the composer's top rule, `    claude ` (a pad, the
/// icon's two cells, a pad, the name, a pad), from the rule's third column,
/// and its lit switch, ` alab harness ● on `, two columns short of the
/// rule's end (`claude_footer::compose_top`, `switch_face`).
const BRAND_MARK_COLS: u16 = 11;
const BRAND_SWITCH_COLS: u16 = 19;

/// One present, as the pet drew it.
#[derive(Clone, Copy, Debug)]
struct Shot {
    ms: u64,
    frame: PetFrame,
    /// The brain's console reason after this tick.
    reason: &'static str,
    /// The guess hull reserved on this present.
    guess: Option<PetRect>,
    /// The caret `(row, col)` on this present.
    caret: (u16, u16),
    /// The composer's bottom rule's row on this present, when one is drawn.
    rule: Option<usize>,
}

struct Host {
    term: Terminal,
    owner: CompanionOwner,
    decos: WordDecorations,
    lexicon: Lexicon,
    cfg: DecoConfig,
    knobs: Knobs,
    rows: u16,
    cols: u16,
    t0: Instant,
    now: Instant,
    /// The display's next vsync.
    vsync: Instant,
    hold: FrameHold,
    /// Pending guesses, oldest first: `(row, col, armed ms)` of each typed
    /// letter.
    guesses: Vec<(u16, u16, u64)>,
    shots: Vec<Shot>,
    input: Option<(u64, RenderInput)>,
}

impl Host {
    fn new(take: &Take, knobs: Knobs) -> Self {
        let mut term = Terminal::new(take.rows, take.cols);
        // A presented host resolves its default background.
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
            knobs,
            rows: take.rows,
            cols: take.cols,
            t0: now,
            now,
            vsync: now + Duration::from_millis(knobs.frame_ms),
            hold: FrameHold::default(),
            guesses: Vec::new(),
            shots: Vec::new(),
            input: None,
        }
    }

    fn ms(&self) -> u64 {
        self.now.saturating_duration_since(self.t0).as_millis() as u64
    }

    /// The predictor's hull of pending guesses (`pending_bounds`), as a rect.
    fn guess_hull(&self) -> Option<PetRect> {
        let (r0, c0, r1, c1) = self
            .guesses
            .iter()
            .map(|&(r, c, _)| (r, c, r + 1, c + 1))
            .reduce(|a, b| (a.0.min(b.0), a.1.min(b.1), a.2.max(b.2), a.3.max(b.3)))?;
        let (r1, c1) = (r1.min(self.rows), c1.min(self.cols));
        (r0 < r1 && c0 < c1).then(|| {
            PetRect::new(
                f32::from(r0),
                f32::from(c0),
                f32::from(r1 - r0),
                f32::from(c1 - c0),
            )
        })
    }

    /// A key, as the app notes it, and the guess the predictor arms for it.
    fn key(&mut self, bytes: &[u8]) {
        match bytes {
            [c] if (0x20..0x7f).contains(c) => {
                let (row, col) = match self.guesses.last() {
                    Some(&(row, col, _)) => (row, (col + 1).min(self.cols - 1)),
                    None => {
                        let c = self.term.cursor();
                        (c.row, c.col)
                    }
                };
                self.guesses.push((row, col, self.ms()));
                self.owner.note_console_input(self.now, PetInputKind::Text);
            }
            [0x7f] => {
                self.guesses.pop();
                self.owner
                    .note_console_input(self.now, PetInputKind::Delete);
            }
            _ => self.guesses.clear(),
        }
    }

    /// Program output; a guess leaves when the echo moves the cursor past it
    /// (a submit, which these keys do not carry, moves it to another row or back
    /// to the line's start; its guesses expire).
    fn feed(&mut self, bytes: &[u8]) {
        let at = ClockReading {
            monotonic: self.now,
            wall_ms: None,
        };
        self.term.process_at(bytes, at);
        let c = self.term.cursor();
        self.guesses
            .retain(|&(row, col, _)| row == c.row && col >= c.col);
    }

    /// The composer's rules: the last two rows drawn mostly in U+2500 (at
    /// least four fifths of the width), `(top, bottom)`.
    fn rules(&self, input: &RenderInput) -> Option<(usize, usize)> {
        let cols = usize::from(self.cols);
        let rules: Vec<usize> = input
            .cells
            .iter()
            .enumerate()
            .filter(|(_, row)| row.iter().filter(|c| c.ch == '─').count() * 5 >= cols * 4)
            .map(|(r, _)| r)
            .collect();
        (rules.len() >= 2).then(|| (rules[rules.len() - 2], rules[rules.len() - 1]))
    }

    /// The native order: observe (with the reserve), prepare, tick, rescan.
    fn present(&mut self) {
        if self.hold.withholds(&self.term, self.now) {
            return;
        }
        let mutations = self.term.screen_mutations();
        if self.input.as_ref().is_none_or(|(at, _)| *at != mutations) {
            let fresh = self
                .term
                .cell_frame(usize::from(self.rows), usize::from(self.cols));
            self.input = Some((mutations, fresh));
        }
        let (_, input) = self.input.as_ref().expect("snapshot taken above");
        let world = PetWorldFacts::read(&self.term, 7);
        let now = self.ms();
        self.guesses
            .retain(|&(_, _, armed)| now < armed + GUESS_EXPIRY_MS);
        let guess = self.guess_hull();
        let typed: Vec<PetRect> = guess.into_iter().collect();
        let rules = self.rules(input);
        let rule = rules.map(|(_, bottom)| bottom);
        let cols = f32::from(self.cols);
        let mut chips: Vec<PetRect> = rule
            .filter(|_| self.knobs.chips)
            .map(|row| {
                let w = f32::from(CHIPS_COLS);
                PetRect::new(row as f32, cols - 1.0 - w, 1.0, w)
            })
            .into_iter()
            .collect();
        if let Some((top, _)) = rules.filter(|_| self.knobs.brand) {
            let (mark, switch) = (f32::from(BRAND_MARK_COLS), f32::from(BRAND_SWITCH_COLS));
            chips.push(PetRect::new(top as f32, 2.0, 1.0, mark));
            chips.push(PetRect::new(top as f32, cols - 2.0 - switch, 1.0, switch));
        }
        if let Some((top, _)) = rules.filter(|_| self.knobs.rule_walled) {
            chips.push(PetRect::new(top as f32, 0.0, 1.0, cols));
        }
        let reserve = match self.knobs.reserve {
            Reserve::Ink => PetReserve {
                protected: &chips,
                overlays: &[],
                ink: &typed,
            },
            // The host until 2026-10-20 (a transient overlay, after main's
            // ground plane): protected on the live plane.
            Reserve::Protected => PetReserve {
                protected: &chips,
                overlays: &typed,
                ink: &[],
            },
        };
        self.owner
            .observe_console_reserved(input, &world, PetPane::full(input), reserve);
        let facts = TerminalFacts::read(&self.term, 7, false);
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
                rows: self.rows,
                cols: self.cols,
                cell_w: CW,
                cell_h: CH,
                origin_px: (0, 0),
            },
        };
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
        let frame = self.owner.tick(tick).pet;
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
        let caret = self.term.cursor();
        self.shots.push(Shot {
            ms: self.ms(),
            frame,
            reason: self.owner.brain().console_reason(),
            guess,
            caret: (caret.row, caret.col),
            rule,
        });
    }

    /// Run the display's frame train up to `t0 + ms`: a present on every
    /// vsync, whatever the program and the keyboard did between them.
    fn run_to(&mut self, ms: u64) {
        let t = self.t0 + Duration::from_millis(ms);
        while self.vsync <= t {
            self.now = self.vsync;
            self.present();
            self.vsync += Duration::from_millis(self.knobs.frame_ms);
        }
        self.now = self.now.max(t);
    }

    /// The whole take up to its exit chord, then a second of quiet. The
    /// program's output is delayed by the knobs' echo latency.
    fn replay(take: &Take, knobs: Knobs) -> Self {
        let mut events: Vec<(u64, Rec)> = Vec::new();
        for (ms, ev) in recording(take.src, 1_000_000) {
            match ev {
                Rec::In(b) if b.as_slice() == b"\x1b[99;5u" => {
                    events.push((ms, Rec::In(b)));
                    break;
                }
                ev => events.push((ms, ev)),
            }
        }
        Self::replay_events(take, knobs, events)
    }

    /// [`Self::replay`] of a take's events, the program's output delayed by
    /// the knobs' echo latency.
    fn replay_events(take: &Take, knobs: Knobs, recorded: Vec<(u64, Rec)>) -> Self {
        let mut h = Self::new(take, knobs);
        let mut events: Vec<(u64, Rec)> = recorded
            .into_iter()
            .map(|(ms, ev)| match ev {
                Rec::Out(b) => (ms + knobs.echo_ms, Rec::Out(b)),
                ev => (ms, ev),
            })
            .collect();
        events.sort_by_key(|(ms, _)| *ms);
        for (ms, ev) in events {
            h.run_to(ms);
            match ev {
                Rec::In(b) if b.as_slice() == b"\x1b[99;5u" => break,
                Rec::In(b) => h.key(&b),
                Rec::Out(b) => h.feed(&b),
            }
        }
        let end = h.ms() + 1_000;
        h.run_to(end);
        h
    }
}

/// A take written here rather than recorded: the events it replays
/// ([`Host::replay_events`]).
type SyntheticTake = fn() -> Vec<(u64, Rec)>;

/// A Claude Code composer under a full transcript, typed across its wrap,
/// written as the program writes it: the transcript's rows inked to the
/// width, the composer between its rules, the footer under it, the caret
/// painted. The hand types a key every 85 ms and the program echoes it 15 ms
/// later; at the right edge the composer grows a line, its bottom rule and
/// the footer pushed a row down. With the transcript full the only clear
/// ground near the composer is under it: a cat whose stand the host's chrome
/// refuses rests there.
fn full_transcript_composer_take() -> Vec<(u64, Rec)> {
    const COLS: usize = 120;
    let mut ev = Vec::new();
    let mut screen = String::from("\x1b[2J\x1b[H");
    for r in 0..19 {
        let line: String = "transcript words rest here on every row "
            .chars()
            .cycle()
            .skip(r * 3)
            .take(COLS - 2)
            .collect();
        screen.push_str(&format!("\x1b[{};1H{line}", r + 1));
    }
    let rule = "─".repeat(COLS);
    screen.push_str(&format!("\x1b[20;1H{rule}\x1b[21;1H❯ \x1b[22;1H{rule}"));
    screen.push_str("\x1b[23;1H  ⏵⏵ auto mode on (shift+tab to cycle)\x1b[21;3H");
    ev.push((0, Rec::Out(screen.into_bytes())));
    let text = "please keep the footer compact while I type this long prompt so that it wraps past the composer's right edge and goes on typing on the line under it";
    let mut t = 1_000;
    let (mut row, mut col) = (21usize, 3usize);
    for ch in text.chars() {
        let b = ch.to_string().into_bytes();
        ev.push((t, Rec::In(b.clone())));
        let echo = if col > COLS {
            // The wrap: the composer grows a line; the rule and the footer
            // go a row down, and the line goes on under the first.
            row += 1;
            col = 3;
            format!(
                "\x1b[{};1H{rule}\x1b[{};1H\x1b[2K  ⏵⏵ auto mode on (shift+tab to cycle)\x1b[{row};1H\x1b[2K  {ch}",
                row + 1,
                row + 2
            )
        } else {
            format!("\x1b[{row};{col}H{ch}")
        };
        col += 1;
        ev.push((t + 15, Rec::Out(echo.into_bytes())));
        t += 85;
    }
    ev
}

/// Claude Code's composer as a windowed host shows it at every key (real
/// Claude Code 2.1.305 at 120 x 40, 2026-10-20): the launch card's rows, the
/// composer between its rules near the top of the pane, the footer under
/// it, blank rows below; the cursor hidden and Claude's own block caret
/// painted after the draft; every repaint one whole DEC 2026 frame, so no
/// present catches a frame half written. The hand types a key every 85 ms
/// and the program's frame lands 15 ms later; at the right edge the composer
/// grows a line, its bottom rule and the footer pushed a row down.
fn bracketed_composer_take() -> Vec<(u64, Rec)> {
    const COLS: usize = 120;
    let rule = "─".repeat(COLS);
    let footer = "  ⏵⏵ auto mode on (shift+tab to cycle)";
    let frame = |draft: &str| {
        let chars: Vec<char> = draft.chars().collect();
        let lines: Vec<String> = if chars.is_empty() {
            vec![String::new()]
        } else {
            chars.chunks(COLS - 2).map(|c| c.iter().collect()).collect()
        };
        let mut out = String::from("\x1b[?2026h");
        for row in 5..12 {
            out.push_str(&format!("\x1b[{};1H\x1b[2K", row + 1));
        }
        out.push_str(&format!("\x1b[5;1H{rule}"));
        for (i, line) in lines.iter().enumerate() {
            let lead = if i == 0 { "❯ " } else { "  " };
            out.push_str(&format!("\x1b[{};1H{lead}{line}", 6 + i));
        }
        let last = lines.len() - 1;
        let caret_col = 2 + lines[last].chars().count();
        out.push_str("\x1b[7m \x1b[27m");
        let lower = 5 + lines.len();
        out.push_str(&format!(
            "\x1b[{};1H{rule}\x1b[{};1H{footer}\x1b[{};{}H\x1b[?2026l",
            lower + 1,
            lower + 2,
            6 + last,
            caret_col + 1
        ));
        out.into_bytes()
    };
    let mut ev = Vec::new();
    let mut screen = String::from("\x1b[2J\x1b[H\x1b[?25l");
    screen.push_str("\x1b[1;1H ▐▛███▜▌   Claude Code v2.1.305");
    screen.push_str("\x1b[2;1H▝▜█████▛▘  Opus 5.5 · API Usage Billing");
    screen.push_str("\x1b[3;1H  ▘▘ ▝▝    /private/tmp/lab/proj");
    ev.push((0, Rec::Out(screen.into_bytes())));
    ev.push((10, Rec::Out(frame(""))));
    let text = "in fact, i would like for the model use sigma section to be hidden by default but can be enabled. i do not really need to know that info at all, and the footer stays";
    let mut t = 1_000;
    for (n, ch) in text.chars().enumerate() {
        ev.push((t, Rec::In(ch.to_string().into_bytes())));
        let draft: String = text.chars().take(n + 1).collect();
        ev.push((t + 15, Rec::Out(frame(&draft))));
        t += 85;
    }
    ev
}

/// What the replay saw of the pet's motion while it was on glass.
#[derive(Debug, Default)]
struct Motion {
    /// Presents with a guess reserved.
    guessing: usize,
    /// Presents whose tick stepped the body aside off a protected surface.
    step_asides: Vec<(u64, f32, f32)>,
    /// Presents whose tick hid the body for want of a place to step to.
    yields: Vec<u64>,
    /// Presents whose tick carried the body down with the rows round it
    /// (`content-ride`): `(ms, dx, dy, rows the bottom rule moved)`.
    rides: Vec<(u64, f32, f32, i64)>,
    /// One-present moves no locomotion makes: half a row or more, or
    /// sideways faster than the top chase speed. A ride is not one.
    jumps: Vec<(u64, f32, f32)>,
    /// The gait art going back to the cycle it left within [`FLICKER_MS`]:
    /// `(ms, the gait it went back to, ms it was away)`.
    flickers: Vec<(u64, &'static str, u64)>,
    /// Every change of the gait art drawn.
    gait_changes: usize,
    /// Presents that draw a gait's art.
    gait_frames: usize,
    /// Runs of presents drawing the run's art while the body crawls (under
    /// [`CREEP_SPEED`] since the present before): `(ms it began, ms it was on
    /// glass)`.
    creeps: Vec<(u64, u64)>,
    /// Runs of presents holding a body that does not move in the run's
    /// action: `(ms it began, ms it was on glass)`.
    stills: Vec<(u64, u64)>,
    /// Runs of presents drawing the stand's pose between two presents of a
    /// leg's art (the walk's, the amble's or the run's): `(ms it began, ms
    /// it was on glass)`.
    stands: Vec<(u64, u64)>,
}

/// Whether a frame draws the stand's pose.
fn standing(frame: &PetFrame) -> bool {
    use aterm_effects::pet_glyphs_gen::PetGlyphId;
    matches!(frame.pose, PetGlyphId::PetStand | PetGlyphId::PetStandEar)
}

/// One stride of the gallop at its top speed (`kitty_pet::STAND_MIN`,
/// `RUN_STRIDE_CELLS / MAX_SPEED`), the quickest stride the legs take, in
/// milliseconds: a stand between two legs shorter than it is a blink.
const STRIDE_MS: f32 = 3.4 / MAX_SPEED * 1_000.0;

/// The gait art a frame draws: the walk's, the amble's or the run's cycle.
fn gait(frame: &PetFrame) -> Option<&'static str> {
    let id = aterm_effects::pet_glyphs_gen::PET_GLYPHS[frame.pose as usize].id;
    ["pet_walk_", "pet_lwalk_", "pet_run_"]
        .into_iter()
        .find(|family| id.starts_with(family))
}

/// Under this speed (cells/s) a body drawn in the run's art crawls.
const CREEP_SPEED: f32 = 3.0;
/// The longest a run's art may be drawn over a crawling body
/// (`kitty_pet::RUN_ART_GRACE`): a dip between two keys that the next one
/// picks up, never a slow-motion gallop.
const CREEP_MAX_MS: u64 = 40;

fn motion(shots: &[Shot], frame_ms: u64) -> Motion {
    let dt = frame_ms as f32 / 1_000.0;
    let mut m = Motion::default();
    // The gait last drawn (a stop between keys draws none), and when each
    // gait's art was last on glass.
    let mut drawn = None;
    let mut seen: std::collections::BTreeMap<&'static str, u64> = Default::default();
    for s in shots {
        let Some(g) = gait(&s.frame) else {
            continue;
        };
        m.gait_frames += 1;
        if drawn.is_some_and(|d| d != g) {
            m.gait_changes += 1;
            if let Some(&last) = seen.get(g)
                && s.ms - last < FLICKER_MS
            {
                m.flickers.push((s.ms, g, s.ms - last));
            }
        }
        seen.insert(g, s.ms);
        drawn = Some(g);
    }
    // Episodes of presents, each as `(first, how long on glass)`.
    let episodes = |hit: &dyn Fn(&Shot, &Shot) -> bool| {
        let mut out = Vec::new();
        let mut from: Option<u64> = None;
        for w in shots.windows(2) {
            match (hit(&w[0], &w[1]), from) {
                (true, None) => from = Some(w[1].ms),
                (false, Some(f)) => {
                    out.push((f, w[0].ms + frame_ms - f));
                    from = None;
                }
                _ => {}
            }
        }
        if let (Some(f), Some(last)) = (from, shots.last()) {
            // Still active at the end: only the span through the final sample is
            // observed. No later present establishes how long it stayed on glass.
            out.push((f, last.ms - f));
        }
        out
    };
    // Stands between two legs: each run of stand presents with a leg's art
    // on glass just before and just after it.
    let mut leg_before = false;
    let mut stand_from: Option<u64> = None;
    for s in shots {
        if gait(&s.frame).is_some() {
            if let Some(from) = stand_from.take() {
                m.stands.push((from, s.ms - from));
            }
            leg_before = true;
        } else if standing(&s.frame) && leg_before {
            stand_from.get_or_insert(s.ms);
        } else {
            leg_before = false;
            stand_from = None;
        }
    }
    m.creeps = episodes(&|a, b| {
        gait(&b.frame) == Some("pet_run_") && (b.frame.col - a.frame.col).abs() / dt < CREEP_SPEED
    });
    m.stills = episodes(&|a, b| {
        b.frame.action == PetAction::Run && (b.frame.col - a.frame.col).abs() < 1e-4
    });
    for w in shots.windows(2) {
        let (prev, s) = (&w[0], &w[1]);
        m.guessing += usize::from(s.guess.is_some());
        let (dx, dy) = (s.frame.col - prev.frame.col, s.frame.row - prev.frame.row);
        match s.reason {
            "cursor-home" => m.step_asides.push((s.ms, dx, dy)),
            "protected-home-unavailable" | "protected-home-rounding" => m.yields.push(s.ms),
            "content-ride" => {
                let moved = match (prev.rule, s.rule) {
                    (Some(a), Some(b)) => b as i64 - a as i64,
                    _ => 0,
                };
                m.rides.push((s.ms, dx, dy, moved));
                continue;
            }
            _ => {}
        }
        // An arc (a hop, a pounce, the wrap's bound) is paid for by the body
        // in the air at its own rate; the ground has the chase's ceiling.
        let airborne = |f: &PetFrame| f.lift != 0.0 || f.action == PetAction::Leap;
        if prev.frame.alpha > 0
            && s.frame.alpha > 0
            && !airborne(&prev.frame)
            && !airborne(&s.frame)
            && (dy.abs() >= 0.5 || dx.abs() > MAX_SPEED * dt + 0.02)
        {
            m.jumps.push((s.ms, dx, dy));
        }
    }
    m
}

/// Every take typed with predictive echo on: the guess never moves the pet.
/// No present steps the body aside, hides it, or moves it on the ground half a
/// row or faster than it can run. Negative control: the same guesses reserved as
/// protected step the body aside and jump it while typing (measured 2026-10-20:
/// 7 step-asides and 6 jumps of 1.3 to 4.9 cells in one present over the four
/// takes, 0 of each reserved as ink).
#[test]
fn a_guessed_letter_never_throws_the_pet_off_its_seat() {
    let runs: Vec<(usize, Reserve)> = [Reserve::Ink, Reserve::Protected]
        .into_iter()
        .flat_map(|reserve| (0..TAKES.len()).map(move |take| (take, reserve)))
        .collect();
    let seen = crate::common::par_map(&runs, |&(take, reserve)| {
        let knobs = Knobs {
            reserve,
            ..Knobs::SHIPPED
        };
        let h = Host::replay(&TAKES[take], knobs);
        (take, reserve, motion(&h.shots, knobs.frame_ms))
    });
    let (mut moved, mut jumped) = (0, 0);
    for (take, reserve, m) in &seen {
        let label = format!("{} {reserve:?}", TAKES[*take].name);
        assert!(m.guessing > 10, "{label}: the take must type ({m:?})");
        if *reserve == Reserve::Protected {
            moved += m.step_asides.len() + m.yields.len();
            jumped += m.jumps.len();
            continue;
        }
        assert!(
            m.step_asides.is_empty() && m.yields.is_empty(),
            "{label}: the guess moved the pet: {} step-asides {:?}, {} yields {:?}",
            m.step_asides.len(),
            &m.step_asides[..m.step_asides.len().min(4)],
            m.yields.len(),
            &m.yields[..m.yields.len().min(4)],
        );
        assert!(
            m.jumps.is_empty(),
            "{label}: {} one-present jumps: {:?}",
            m.jumps.len(),
            &m.jumps[..m.jumps.len().min(4)]
        );
    }
    assert!(
        moved > 0 && jumped > 0,
        "negative control: a protected guess must step the pet aside and jump it \
         ({moved} step-asides and yields, {jumped} jumps): {seen:?}"
    );
}

/// The legs keep one gait while the person types, at 120 and at 60 Hz, with
/// the echo as recorded and 150 ms late. The chase speed pulses with every key
/// (a spring past the run's threshold, a collapse on arrival, a stall at a
/// late key), and the gait used to follow each pulse: the drawn legs swapped
/// between the amble's, the walk's and the run's art up to three times in
/// 24 ms (measured 2026-10-20 over these four takes: 851 changes, 739 of them
/// back to a gait left less than 350 ms before). Read through a held peak of
/// the speed instead, the legs still went back to a gait at a late key, and a
/// cat that landed from a sprint crept the cell after it in a gallop for
/// 430 ms. The gait reads the speed averaged over `kitty_pet::GAIT_TAU`, is
/// worn at least `GAIT_DWELL`, and picks afresh after a rest or a flight: no
/// present goes back to a gait's art within 350 ms of last drawing it.
///
/// Inside the run the body slows into each stop and sets out from it again
/// (the walk-on between keys coasting down when a key is late), and the
/// run's distance-driven frames froze in a mid-gallop pose while it crawled:
/// up to 128 ms of slow gallop per stop, and at the right margin 104 ms of a
/// body standing still in the run's frame (a station pinned wall-side read as
/// walking on). Under 3 cells/s the run's feet are planted
/// (`kitty_pet::RUN_ART_MIN`) once the body has crawled
/// `kitty_pet::RUN_ART_GRACE`, and the station pinned there is arrived at: no
/// present draws the run's art over a crawling body for longer than
/// [`CREEP_MAX_MS`] (and the present it runs out on), and none holds a still
/// body in the run for more than one present. The gait itself is untouched:
/// ended under a hand at a walking pace instead, the run came back over the
/// walk's ceiling a few hundred milliseconds later at the next surge.
#[test]
fn the_legs_keep_one_gait_while_typing() {
    // A stuck run at the end of a take must count too. Start from one ordinary
    // host frame, then hold its running body through the last observed sample.
    let mut control = Host::new(&TAKES[0], Knobs::SHIPPED);
    control.present();
    let first = control.shots[0];
    let running = PetFrame {
        alpha: 255,
        lane_alpha: 255,
        action: PetAction::Run,
        pose: aterm_effects::pet_glyphs_gen::PetGlyphId::PetRun0,
        col: 4.0,
        row: 3.0,
        ..first.frame
    };
    let mut tail: Vec<Shot> = (0..=8)
        .map(|i| Shot {
            ms: i * 8,
            frame: running,
            ..first
        })
        .collect();
    let open = motion(&tail, 8);
    assert_eq!(open.creeps, [(8, 56)], "through the last sample at 64 ms");
    assert_eq!(open.stills, [(8, 56)], "a final stuck run is observed");
    tail.push(Shot {
        ms: 72,
        frame: PetFrame {
            action: PetAction::Stand,
            pose: aterm_effects::pet_glyphs_gen::PetGlyphId::PetStand,
            ..running
        },
        ..first
    });
    let closed = motion(&tail, 8);
    assert_eq!(closed.creeps, [(8, 64)], "the next sample ends the episode");
    assert_eq!(closed.stills, [(8, 64)]);

    let runs: Vec<(usize, u64, u64)> = (0..TAKES.len())
        .flat_map(|take| {
            [(8, 0), (16, 0), (8, 150), (16, 150)]
                .into_iter()
                .map(move |(frame_ms, echo_ms)| (take, frame_ms, echo_ms))
        })
        .collect();
    let seen = crate::common::par_map(&runs, |&(take, frame_ms, echo_ms)| {
        let knobs = Knobs {
            frame_ms,
            echo_ms,
            ..Knobs::SHIPPED
        };
        let h = Host::replay(&TAKES[take], knobs);
        (take, frame_ms, echo_ms, motion(&h.shots, frame_ms))
    });
    for (take, frame_ms, echo_ms, m) in &seen {
        let label = format!(
            "{} at {frame_ms} ms a frame, echo +{echo_ms} ms",
            TAKES[*take].name
        );
        assert!(
            m.gait_frames > 50,
            "{label}: the cat must walk or run ({m:?})"
        );
        assert!(
            m.flickers.is_empty(),
            "{label}: {} of {} gait changes went back within {FLICKER_MS} ms: {:?}",
            m.flickers.len(),
            m.gait_changes,
            &m.flickers[..m.flickers.len().min(6)]
        );
        let creep = m.creeps.iter().max_by_key(|c| c.1);
        assert!(
            creep.is_none_or(|c| c.1 <= CREEP_MAX_MS + *frame_ms),
            "{label}: the run's art was drawn over a body crawling under {CREEP_SPEED} \
             cells/s for {creep:?} (ms it began, ms on glass)"
        );
        let still = m.stills.iter().max_by_key(|c| c.1);
        assert!(
            still.is_none_or(|c| c.1 <= *frame_ms),
            "{label}: a still body held the run for {still:?} (ms it began, ms on glass)"
        );
    }
}

/// A STAND BETWEEN TWO LEGS LASTS A STRIDE: wherever the stand's pose is
/// drawn between two presents of a leg's art, it is on glass at least one
/// stride of the gallop ([`STRIDE_MS`], `kitty_pet::STAND_MIN`), over every
/// take at 120 and 60 Hz, echo as recorded and 150 ms late. A run that all
/// but stopped between two late keys planted its feet, or arrived and stood,
/// and set out again a frame or a few later: measured 2026-10-21 over these
/// takes, 10 to 12 such stands in each generating replay under a stride,
/// from 8 to 128 ms (a walk stood one present; a run stood 40 to 72 ms with
/// no arrival at all), the "standing blinks". The stand now holds the body
/// where it stopped for a stride, then the leg sets out as before; the gait
/// laws of [`the_legs_keep_one_gait_while_typing`] stand.
#[test]
fn a_stand_between_two_legs_lasts_a_stride() {
    // The measure: a stand bounded by legs is counted, one bounded by
    // anything else is not.
    let mut control = Host::new(&TAKES[0], Knobs::SHIPPED);
    control.present();
    let first = control.shots[0];
    let with = |pose| Shot {
        frame: PetFrame {
            pose,
            ..first.frame
        },
        ..first
    };
    use aterm_effects::pet_glyphs_gen::PetGlyphId::{PetRun0, PetSit, PetStand, PetWalk1};
    let shots: Vec<Shot> = [
        PetRun0, PetStand, PetStand, PetWalk1, PetStand, PetSit, PetRun0,
    ]
    .into_iter()
    .enumerate()
    .map(|(i, pose)| Shot {
        ms: i as u64 * 8,
        ..with(pose)
    })
    .collect();
    assert_eq!(motion(&shots, 8).stands, [(8, 16)]);

    let runs: Vec<(usize, u64, u64)> = (0..TAKES.len())
        .flat_map(|take| {
            [(8, 0), (16, 0), (8, 150), (16, 150)]
                .into_iter()
                .map(move |(frame_ms, echo_ms)| (take, frame_ms, echo_ms))
        })
        .collect();
    let seen = crate::common::par_map(&runs, |&(take, frame_ms, echo_ms)| {
        let knobs = Knobs {
            frame_ms,
            echo_ms,
            ..Knobs::SHIPPED
        };
        let h = Host::replay(&TAKES[take], knobs);
        (take, frame_ms, echo_ms, motion(&h.shots, frame_ms))
    });
    let mut stood = 0;
    for (take, frame_ms, echo_ms, m) in &seen {
        let label = format!(
            "{} at {frame_ms} ms a frame, echo +{echo_ms} ms",
            TAKES[*take].name
        );
        stood += m.stands.len();
        let blinks: Vec<&(u64, u64)> = m
            .stands
            .iter()
            .filter(|(_, on_glass)| (*on_glass as f32) < STRIDE_MS)
            .collect();
        assert!(
            blinks.is_empty(),
            "{label}: {} of {} stands between two legs were on glass under a stride \
             ({STRIDE_MS:.0} ms): {blinks:?} (ms it began, ms on glass)",
            blinks.len(),
            m.stands.len()
        );
    }
    assert!(stood > 40, "the takes stop between legs ({stood} stands)");
}

/// The composer grows a line under a cat that rests beside aterm's footer
/// chips. Typing a long prompt in Claude Code with the composer's upper rule
/// reserved whole (the harness's tint was, until 2026-10-20: `Knobs::
/// rule_walled`), the stand on the composer's first line reaches into the
/// upper rule, so the cat rests under the composer, its head beside the
/// chips' rule; at the wrap the composer grows a line, and the rule and the
/// chips come down onto the cat's head. The console law stepped it aside: in one
/// present, 28 px down and 19 px left at the owner's 15x28 cell (2026-10-20,
/// review of `fix/kitty-jitter`, the generating take at every wrap). Now the
/// body rides down with the rows round it, the rows they moved and not a
/// column sideways: no present steps it aside or hides it, every ride moves it
/// exactly the rows the composer's rule moved, and nothing else moves it on
/// the ground faster than it can run. Negative control: the same take at 22
/// rows, where the composer never grows under the cat, rides nothing.
#[test]
fn the_pet_rides_the_composer_down_when_its_chips_come_onto_it() {
    let rows: Vec<u16> = vec![40, 26, 22];
    let seen = crate::common::par_map(&rows, |&rows| {
        let take = Take { rows, ..TAKES[0] };
        let knobs = Knobs {
            chips: true,
            rule_walled: true,
            ..Knobs::SHIPPED
        };
        let h = Host::replay(&take, knobs);
        (rows, motion(&h.shots, knobs.frame_ms))
    });
    for (rows, m) in &seen {
        let label = format!("generating at {rows} rows with the chips");
        assert!(
            m.step_asides.is_empty() && m.yields.is_empty(),
            "{label}: {} step-asides {:?}, {} yields {:?}",
            m.step_asides.len(),
            &m.step_asides[..m.step_asides.len().min(4)],
            m.yields.len(),
            &m.yields[..m.yields.len().min(4)],
        );
        assert!(
            m.jumps.is_empty(),
            "{label}: {} one-present jumps: {:?}",
            m.jumps.len(),
            &m.jumps[..m.jumps.len().min(4)]
        );
        for &(ms, dx, dy, moved) in &m.rides {
            assert!(
                dx == 0.0 && (dy - moved as f32).abs() < 1e-3 && moved > 0,
                "{label}: the ride at {ms} ms moved the body ({dx}, {dy}) while the \
                 composer's rule moved {moved} rows"
            );
        }
        if *rows == 22 {
            assert!(m.rides.is_empty(), "{label}: rides {:?}", m.rides);
        } else {
            assert!(
                !m.rides.is_empty(),
                "{label}: the wrap grows the composer under the resting cat: rides {:?}",
                m.rides
            );
        }
    }
}

/// The cat walks the composer's line beside the caret, over aterm's footer
/// chips. With the chips on the composer's lower rule (the owner's footer),
/// the stand beside the caret on the composer's line has its paws on the
/// rule's top edge over the chips. Judged with the clearance margin under
/// its feet too, the stand was refused from the chips' first column on: the
/// cat dropped under the composer at that column and waited there, fifty
/// cells behind, while the line was typed to its end (2026-10-20: real
/// Claude Code 2.1.305, windowed, a typed line's lag at its median 22 cells
/// and 53 at its 90th percentile). Now, typed on to the wrap, the cat stays
/// on the composer's line and beside the caret: at every present after the
/// chips' first column, its feet on the caret's row and its body within
/// [`STAND_REACH`] columns of the caret.
#[test]
fn the_pet_walks_the_composer_line_beside_its_caret_over_the_chips() {
    let knobs = Knobs {
        chips: true,
        ..Knobs::SHIPPED
    };
    let h = Host::replay(&TAKES[0], knobs);
    let chips_from = h.cols - 1 - CHIPS_COLS;
    // The composer's first line, from the chips' first column to the wrap.
    let line: Vec<&Shot> = h
        .shots
        .iter()
        .skip_while(|s| s.caret.1 < chips_from)
        .take_while(|s| s.caret.1 >= chips_from)
        .filter(|s| s.frame.alpha > 0 && s.frame.action != PetAction::Leap)
        .collect();
    assert!(
        line.len() > 100,
        "the take types the line on past the chips: {}",
        line.len()
    );
    let off: Vec<(u64, (u16, u16), f32, f32)> = line
        .iter()
        .filter(|s| {
            (s.frame.row - f32::from(s.caret.0)).abs() >= 0.5
                || (s.frame.col - f32::from(s.caret.1)).abs() > STAND_REACH
        })
        .map(|s| (s.ms, s.caret, s.frame.col, s.frame.row))
        .collect();
    assert!(
        off.is_empty(),
        "{} presents off the caret's line or farther than {STAND_REACH} columns from it: {:?}",
        off.len(),
        &off[..off.len().min(6)]
    );
}

/// How far (columns) a cat walking beside the caret may be from it: the
/// escort's lead past the caret (`kitty_pet::STATION_LEAD` and the keep-ahead,
/// at most `LEAD_MAX`), its own width, and its lag behind a burst.
const STAND_REACH: f32 = 12.0;

/// A wrap typed through is one bound, whatever the host draws round the
/// composer. At the composer's right edge the caret wraps to the next line
/// and the person types on; the cat bounds the width of the composer to it.
/// The bound was aimed once, at launch, and the hand typed some ten cells on
/// through its second in the air: the cat came down nine cells behind the
/// caret, and the jump it had latched at the wrap launched it again
/// (2026-10-20, review of `fix/kitty-jitter`: about 3 s of leaping at every
/// wrap). The landing now tracks the writing in the air, and the flight
/// spends the latch it was launched for.
///
/// With aterm's chips on the composer's rule and its upper rule reserved
/// whole (`Knobs::rule_walled`), the cat rests under the composer while its
/// first line is typed, and the bound up to the new line passes over the
/// chips: walled off by them (the box between the bodies cutting their
/// corner) it went in two flights, down to a seat it could reach and then
/// up, and kept off them in the air it glided with no rise. With the
/// harness's lockup and switch on the upper rule (`Knobs::brand`), the cat
/// walks the first line and waits short of the switch, and gathers for the
/// bound there without a step aside. And in Claude Code as a window shows it,
/// every frame one closed DEC 2026 bracket, the hidden caret it finished at
/// was taken two presents late (`companion::HIDDEN_REST`, kept for moves no
/// bracket closes): at the wrap the stale caret sent the cat hopping down
/// to the new line's far end before it bounded back. After each wrap, while
/// the caret stays on the new line: one flight, and it comes down ahead of
/// the caret.
#[test]
fn a_wrap_typed_through_is_one_bound() {
    let harness = Knobs {
        chips: true,
        brand: true,
        ..Knobs::SHIPPED
    };
    let chips = Knobs {
        chips: true,
        ..Knobs::SHIPPED
    };
    let walled = Knobs {
        chips: true,
        rule_walled: true,
        ..Knobs::SHIPPED
    };
    let window = Take {
        name: "bracketed",
        src: "",
        rows: 40,
        cols: 120,
    };
    let runs: [(&str, Knobs, Option<SyntheticTake>); 9] = [
        ("generating", Knobs::SHIPPED, None),
        ("generating, chips", chips, None),
        ("generating, harness", harness, None),
        ("generating, rule walled", walled, None),
        (
            "windowed frames",
            Knobs::SHIPPED,
            Some(bracketed_composer_take),
        ),
        (
            "windowed frames, chips",
            chips,
            Some(bracketed_composer_take),
        ),
        (
            "windowed frames, harness",
            harness,
            Some(bracketed_composer_take),
        ),
        (
            "full transcript, harness",
            harness,
            Some(full_transcript_composer_take),
        ),
        (
            "windowed frames, rule walled",
            walled,
            Some(bracketed_composer_take),
        ),
    ];
    let cases = crate::common::par_map(&runs, |&(name, knobs, events)| {
        let h = match events {
            None => Host::replay(&TAKES[0], knobs),
            Some(events) => Host::replay_events(&window, knobs, events()),
        };
        (name, h.cols, h.shots)
    });
    for (name, cols, shots) in &cases {
        // Nothing on the ground is stepped aside: a coil gathering for the
        // bound beside the harness's switch spread over the margin its stand
        // kept, and the console law jumped it sideways (judged at its drawn
        // size, not its own: 0.4 columns in one present).
        let m = motion(shots, 8);
        assert!(
            m.step_asides.is_empty() && m.yields.is_empty(),
            "{name}: {} step-asides {:?}, {} yields {:?}",
            m.step_asides.len(),
            &m.step_asides[..m.step_asides.len().min(4)],
            m.yields.len(),
            &m.yields[..m.yields.len().min(4)]
        );
        let wraps: Vec<usize> = shots
            .windows(2)
            .enumerate()
            .filter(|(_, w)| {
                w[1].caret.0 == w[0].caret.0 + 1 && w[0].caret.1 + 10 >= *cols && w[1].caret.1 < 10
            })
            .map(|(i, _)| i + 1)
            .collect();
        assert!(!wraps.is_empty(), "{name}: the take must wrap: {wraps:?}");
        for &at in &wraps {
            // From the present before the wrap (a flight launched on the
            // wrap's own present is the wrap's) for three seconds, while the
            // caret stays on the line it wrapped to (a submit after it sends
            // the cat elsewhere).
            let (start, line) = (shots[at].ms, shots[at].caret.0);
            let after: Vec<&Shot> = std::iter::once(&shots[at - 1])
                .chain(
                    shots
                        .iter()
                        .skip(at)
                        .take_while(|s| s.ms <= start + 3_000 && s.caret.0 == line),
                )
                .collect();
            let launches: Vec<(u64, f32, f32)> = after
                .windows(2)
                .filter(|w| {
                    w[1].frame.action == PetAction::Leap && w[0].frame.action != PetAction::Leap
                })
                .map(|w| (w[1].ms, w[1].frame.col, w[1].frame.row))
                .collect();
            assert_eq!(
                launches.len(),
                1,
                "{name}: the wrap at {start} ms: flights (ms, col, row) {launches:?}"
            );
            let landed = after
                .windows(2)
                .find(|w| {
                    w[0].frame.action == PetAction::Leap && w[1].frame.action != PetAction::Leap
                })
                .map(|w| w[1])
                .expect("the bound lands");
            assert!(
                landed.frame.col >= f32::from(landed.caret.1),
                "{name}: the wrap at {start} ms: the bound came down at col {:.2}, behind the \
                 caret at {:?}",
                landed.frame.col,
                landed.caret
            );
        }
    }
}
