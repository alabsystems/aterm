// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! The pet types beside its guess.

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
/// The fastest the pet ever gallops (`kitty_pet::TOP_SPEED_MAX`, columns per
/// second): the chase follows a live caret run's pace past [`MAX_SPEED`].
const TOP_SPEED: f32 = 48.0;
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
/// stamped when aterm had written it).
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
    /// whole run).
    brand: bool,
    /// The composer's upper rule reserved from end to end, as the host
    /// reserved its tint until 2026-10-20 (`PetChrome::note_painted`): no
    /// stand on the composer's first line, so the cat rests under the
    /// composer while it is typed, beside the chips.
    rule_walled: bool,
    /// The composer's first line itself reserved from end to end: no stand
    /// on it for the escort either, so the cat rests under the composer,
    /// beside the chips, as `rule_walled` sent it until the escort passed
    /// under the chrome over its line.
    line_walled: bool,
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
        line_walled: false,
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
        if let Some((top, _)) = rules.filter(|_| self.knobs.line_walled) {
            chips.push(PetRect::new(top as f32 + 1.0, 0.0, 1.0, cols));
        }
        let reserve = match self.knobs.reserve {
            Reserve::Ink => PetReserve {
                protected: &chips,
                overlays: &[],
                ink: &typed,
            },
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
/// painted.
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
/// present catches a frame half written.
fn bracketed_composer_take() -> Vec<(u64, Rec)> {
    bracketed_composer_typing(
        "in fact, i would like for the model use sigma section to be hidden by default but can be enabled. i do not really need to know that info at all, and the footer stays",
        0,
    )
}

/// [`bracketed_composer_take`]'s composer with `text` typed into it, then
/// `rest` milliseconds with no key.
fn bracketed_composer_typing(text: &str, rest: u64) -> Vec<(u64, Rec)> {
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
    let mut t = 1_000;
    for (n, ch) in text.chars().enumerate() {
        ev.push((t, Rec::In(ch.to_string().into_bytes())));
        let draft: String = text.chars().take(n + 1).collect();
        ev.push((t + 15, Rec::Out(frame(&draft))));
        t += 85;
    }
    if rest > 0 {
        ev.push((t + rest, Rec::Out(Vec::new())));
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
    /// sideways faster than the fastest gallop ([`TOP_SPEED`]).
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
            out.push((f, last.ms - f));
        }
        out
    };
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
        let airborne = |f: &PetFrame| f.lift != 0.0 || f.action == PetAction::Leap;
        if prev.frame.alpha > 0
            && s.frame.alpha > 0
            && !airborne(&prev.frame)
            && !airborne(&s.frame)
            && (dy.abs() >= 0.5 || dx.abs() > TOP_SPEED * dt + 0.02)
        {
            m.jumps.push((s.ms, dx, dy));
        }
    }
    m
}

/// Every take typed with predictive echo on: the guess never moves the pet.
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
/// the echo as recorded and 150 ms late.
#[test]
fn the_legs_keep_one_gait_while_typing() {
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
/// take at 120 and 60 Hz, echo as recorded and 150 ms late.
#[test]
fn a_stand_between_two_legs_lasts_a_stride() {
    let mut control = Host::new(&TAKES[0], Knobs::SHIPPED);
    control.present();
    let first = control.shots[0];
    let with = |pose| {
        let mut shot = first;
        shot.frame.pose = pose;
        shot
    };
    use aterm_effects::pet_glyphs_gen::PetGlyphId::{PetRun0, PetSit, PetStand, PetWalk1};
    let shots: Vec<Shot> = [
        PetRun0, PetStand, PetStand, PetWalk1, PetStand, PetSit, PetRun0,
    ]
    .into_iter()
    .enumerate()
    .map(|(i, pose)| {
        let mut shot = with(pose);
        shot.ms = i as u64 * 8;
        shot
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
/// chips.
#[test]
fn the_pet_rides_the_composer_down_when_its_chips_come_onto_it() {
    let rows: Vec<u16> = vec![40, 26, 22];
    let seen = crate::common::par_map(&rows, |&rows| {
        let take = Take { rows, ..TAKES[0] };
        let knobs = Knobs {
            chips: true,
            line_walled: true,
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
/// chips.
#[test]
fn the_pet_walks_the_composer_line_beside_its_caret_over_the_chips() {
    let knobs = Knobs {
        chips: true,
        ..Knobs::SHIPPED
    };
    let h = Host::replay(&TAKES[0], knobs);
    let chips_from = h.cols - 1 - CHIPS_COLS;
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

/// The cat's width in tenths of a column at the owner's cell: the 1.7-row
/// body at its aspect, `1.7 × 28 × 244 / 148 / 15` (`kitty_pet::art_cols`).
const BODY_COLS_X10: u16 = 52;

/// How far (columns) a cat walking beside the caret may be from it: the
/// escort's lead past the caret (`kitty_pet::STATION_LEAD` and the keep-ahead,
/// at most `LEAD_MAX`), its own width, and its lag behind a burst.
const STAND_REACH: f32 = 12.0;

/// A wrap typed through is one bound, whatever the host draws round the
/// composer.
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
            let (start, line) = (shots[at].ms, shots[at].caret.0);
            let from_row = shots[at - 1].caret.0;
            let body = f32::from(BODY_COLS_X10) / 10.0;
            let mut zone = at - 1;
            while zone > 0
                && shots[zone - 1].caret.0 == from_row
                && f32::from(shots[zone - 1].caret.1) + 1.0 + body > f32::from(*cols)
            {
                zone -= 1;
            }
            let after: Vec<&Shot> = shots[zone.saturating_sub(1)..at]
                .iter()
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

/// A FLIGHT ACROSS THE HOST'S CHROME NEVER BLINKS OUT. With aterm's chips on
/// the composer's lower rule and the harness's mark and switch on its upper
/// one, the bound that follows Claude Code's caret along the composer at the
/// take's start crosses the switch for longer than the visibility hold's
/// dwell.
#[test]
fn a_flight_across_the_hosts_chrome_never_blinks_out() {
    let harness = Knobs {
        chips: true,
        brand: true,
        ..Knobs::SHIPPED
    };
    let chips = Knobs {
        chips: true,
        ..Knobs::SHIPPED
    };
    let window = Take {
        name: "bracketed",
        src: "",
        rows: 40,
        cols: 120,
    };
    let runs: [(&str, Knobs, Option<SyntheticTake>); 6] = [
        ("generating", Knobs::SHIPPED, None),
        ("generating, chips", chips, None),
        ("generating, harness", harness, None),
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
            "windowed frames",
            Knobs::SHIPPED,
            Some(bracketed_composer_take),
        ),
    ];
    let cases = crate::common::par_map(&runs, |&(name, knobs, events)| {
        let h = match events {
            None => Host::replay(&TAKES[0], knobs),
            Some(events) => Host::replay_events(&window, knobs, events()),
        };
        (name, h.shots)
    });
    let mut flights = 0;
    for (name, shots) in &cases {
        let mut blank: Vec<u64> = Vec::new();
        let mut lit_launch = false;
        for w in shots.windows(2) {
            let (prev, s) = (&w[0], &w[1]);
            if s.frame.action == PetAction::Leap && prev.frame.action != PetAction::Leap {
                lit_launch = prev.frame.lane_alpha > 0;
                flights += usize::from(lit_launch);
            }
            if s.frame.action == PetAction::Leap && lit_launch && s.frame.lane_alpha == 0 {
                blank.push(s.ms);
            }
        }
        assert!(
            blank.is_empty(),
            "{name}: {} presents in the air drew the cat at zero presence: {:?}",
            blank.len(),
            &blank[..blank.len().min(8)]
        );
    }
    assert!(flights > 10, "the takes fly ({flights} flights)");
}

/// THE CAT WALKS THE COMPOSER'S LINE BESIDE ITS CARET UNDER THE HARNESS
/// SWITCH.
#[test]
fn the_pet_walks_the_composer_line_beside_its_caret_under_the_harness_switch() {
    let knobs = Knobs {
        chips: true,
        brand: true,
        ..Knobs::SHIPPED
    };
    let h = Host::replay(&TAKES[0], knobs);
    let switch_from = h.cols - 2 - BRAND_SWITCH_COLS;
    let line: Vec<&Shot> = h
        .shots
        .iter()
        .skip_while(|s| s.caret.1 < switch_from)
        .take_while(|s| s.caret.1 >= switch_from)
        .filter(|s| s.frame.alpha > 0 && s.frame.action != PetAction::Leap)
        .collect();
    assert!(
        line.len() > 100,
        "the take types the line on past the switch: {}",
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

/// THE CAT NEVER PARKS ON THE HOST'S CHROME. Typed up under the harness
/// switch, the cat walks the composer's line beside the caret, passing under
/// the switch; the hand stops there and the cat comes to rest.
#[test]
fn the_pet_never_parks_under_the_hosts_chrome() {
    let knobs = Knobs {
        chips: true,
        brand: true,
        ..Knobs::SHIPPED
    };
    let window = Take {
        name: "bracketed",
        src: "",
        rows: 40,
        cols: 120,
    };
    let words = "the quick brown fox jumps over the lazy dog ".repeat(3);
    for (typed, under) in [(100usize, true), (60, false)] {
        let text: String = words.chars().take(typed).collect();
        let h = Host::replay_events(&window, knobs, bracketed_composer_typing(&text, 4_000));
        let last_key = 1_000 + 85 * (typed as u64 - 1) + 15;
        let rest: Vec<&Shot> = h.shots.iter().filter(|s| s.ms > last_key).collect();
        let flights = rest
            .windows(2)
            .filter(|w| {
                w[1].frame.action == PetAction::Leap && w[0].frame.action != PetAction::Leap
            })
            .count();
        let end = rest.last().expect("the rest is presented");
        let switch = (
            f32::from(window.cols) - 2.0 - f32::from(BRAND_SWITCH_COLS),
            f32::from(window.cols) - 2.0,
        );
        let (x0, x1, y0, _) = end
            .frame
            .body_px(CW, CH, window.cols, window.rows)
            .expect("the cat is on glass");
        let (c0, c1) = (x0 as f32 / f32::from(CW), x1 as f32 / f32::from(CW));
        let head_row = y0.div_euclid(i32::from(CH)) as usize;
        let top_rule = end.rule.map(|r| r - 2);
        let under_switch = top_rule == Some(head_row) && c1 > switch.0 && c0 < switch.1;
        assert_eq!(
            flights, 0,
            "typed to {typed}: the rest is walked to: {flights} flights"
        );
        assert!(
            end.frame.action.settled() && !under_switch,
            "typed to {typed}: at rest {:?} at ({:.2}, {}), cols {c0:.2}..{c1:.2} under the switch \
             ({:.0}..{:.0} on row {top_rule:?})",
            end.frame.action,
            end.frame.col,
            end.frame.row,
            switch.0,
            switch.1
        );
        if !under {
            assert!(
                (end.frame.col - typed as f32).abs() < STAND_REACH,
                "control: stopped short of the switch, it rests by the caret (col {:.2})",
                end.frame.col
            );
        }
    }
}

/// The shortest time a pose may be on glass between two others
/// (`kitty_pet::STILL_SECS`: two 60 Hz frames less half of one), in
/// milliseconds: two presents at 60 Hz, four at 120 Hz.
const POSE_MS: u64 = 25;

/// Each pose run between two others as `(ms it began, before, pose, after, ms
/// on glass)`, by the art's family: a leg's cycle (the walk's, the amble's,
/// the run's) is one family, as are the flight's frames and the gather's.
fn pose_runs(shots: &[Shot]) -> Vec<(u64, String, String, String, u64)> {
    let family = |s: &Shot| {
        let id = aterm_effects::pet_glyphs_gen::PET_GLYPHS[s.frame.pose as usize].id;
        match id
            .trim_end_matches(|c: char| c.is_ascii_digit())
            .trim_end_matches('_')
        {
            "pet_walk" | "pet_lwalk" | "pet_run" => "leg".to_string(),
            "pet_launch" | "pet_leap_rise" | "pet_leap" | "pet_hop" | "pet_apex"
            | "pet_leap_descend" => "air".to_string(),
            "pet_crouch" | "pet_crouch_wiggle" => "crouch".to_string(),
            other => other.to_string(),
        }
    };
    let shots: Vec<&Shot> = shots.iter().filter(|s| s.frame.alpha > 0).collect();
    let fams: Vec<String> = shots.iter().map(|s| family(s)).collect();
    let mut runs = Vec::new();
    let mut i = 1;
    while i < shots.len() {
        if fams[i] == fams[i - 1] {
            i += 1;
            continue;
        }
        let mut j = i;
        while j + 1 < shots.len() && fams[j + 1] == fams[i] {
            j += 1;
        }
        if j + 1 < shots.len() {
            runs.push((
                shots[i].ms,
                fams[i - 1].clone(),
                fams[i].clone(),
                fams[j + 1].clone(),
                shots[j + 1].ms - shots[i].ms,
            ));
        }
        i = j + 1;
    }
    runs
}

/// NO POSE IS A ONE-FRAME FLASH: wherever the cat changes pose, the pose it
/// takes is on glass at least [`POSE_MS`] before the next one, over every take
/// at 120 and 60 Hz, echo as recorded and 150 ms late, with and without the
/// harness's chrome; and a stand beside a leg lasts a stride ([`STRIDE_MS`]).
#[test]
fn no_pose_is_a_one_frame_flash() {
    let mut control = Host::new(&TAKES[0], Knobs::SHIPPED);
    control.present();
    let first = control.shots[0];
    use aterm_effects::pet_glyphs_gen::PetGlyphId::{PetRun0, PetRun1, PetSit, PetStand};
    let shots: Vec<Shot> = [PetSit, PetRun0, PetRun1, PetStand, PetSit, PetSit]
        .into_iter()
        .enumerate()
        .map(|(i, pose)| {
            let mut shot = first;
            shot.ms = i as u64 * 8;
            shot.frame.pose = pose;
            shot.frame.alpha = 255;
            shot
        })
        .collect();
    let measured: Vec<(u64, &str, u64)> = pose_runs(&shots)
        .iter()
        .map(|(at, _, pose, _, dur)| (*at, if pose == "leg" { "leg" } else { "stand" }, *dur))
        .collect();
    assert_eq!(measured, [(8, "leg", 16), (24, "stand", 8)]);

    let harness = Knobs {
        chips: true,
        brand: true,
        ..Knobs::SHIPPED
    };
    let runs: Vec<(usize, u64, u64, Knobs)> = (0..TAKES.len())
        .flat_map(|take| {
            [(8, 0), (16, 0), (8, 150), (16, 150)].into_iter().flat_map(
                move |(frame_ms, echo_ms)| {
                    [Knobs::SHIPPED, harness].into_iter().map(move |k| {
                        (
                            take,
                            frame_ms,
                            echo_ms,
                            Knobs {
                                frame_ms,
                                echo_ms,
                                ..k
                            },
                        )
                    })
                },
            )
        })
        .collect();
    let seen = crate::common::par_map(&runs, |&(take, frame_ms, echo_ms, knobs)| {
        let h = Host::replay(&TAKES[take], knobs);
        (take, frame_ms, echo_ms, knobs.brand, pose_runs(&h.shots))
    });
    let mut changes = 0;
    for (take, frame_ms, echo_ms, brand, runs) in &seen {
        changes += runs.len();
        let flashes: Vec<_> = runs
            .iter()
            .filter(|(_, before, pose, after, dur)| {
                let stand = pose == "pet_stand" || pose == "pet_stand_ear";
                *dur < POSE_MS
                    || (stand && (before == "leg" || after == "leg") && (*dur as f32) < STRIDE_MS)
            })
            .collect();
        assert!(
            flashes.is_empty(),
            "{} at {frame_ms} ms a frame, echo +{echo_ms} ms{}: {} of {} poses flashed \
             (ms it began, before, pose, after, ms on glass): {:?}",
            TAKES[*take].name,
            if *brand { ", harness" } else { "" },
            flashes.len(),
            runs.len(),
            &flashes[..flashes.len().min(6)]
        );
    }
    assert!(changes > 400, "the takes change pose ({changes} changes)");
}

/// Each place the body on the ground turned back within 600 ms of setting out
/// (half a cell or more each way), while the caret went one way only from
/// 600 ms before to 300 ms after: `(ms it set out, from col, to col)`.
fn turns_against_the_writing(shots: &[Shot]) -> Vec<(u64, f32, f32)> {
    let ground: Vec<&Shot> = shots
        .iter()
        .filter(|s| {
            s.frame.alpha > 0
                && s.frame.lift == 0.0
                && !matches!(s.frame.action, PetAction::Leap | PetAction::Land)
        })
        .collect();
    let at = |s: &Shot| f32::from(s.caret.0) * 1_000.0 + f32::from(s.caret.1);
    let mut turns = Vec::new();
    let Some(first) = ground.first() else {
        return turns;
    };
    let (mut dir, mut anchor, mut since) = (0.0f32, first.frame.col, first.ms);
    for (i, g) in ground.iter().enumerate().skip(1) {
        let prev = ground[i - 1];
        if g.ms - prev.ms > 100 || (g.frame.row - prev.frame.row).abs() > 0.3 {
            (dir, anchor, since) = (0.0, g.frame.col, g.ms);
            continue;
        }
        let d = g.frame.col - anchor;
        if d.abs() < 0.5 {
            continue;
        }
        if dir != 0.0 && d.signum() != dir && g.ms - since <= 600 {
            let window: Vec<f32> = shots
                .iter()
                .filter(|s| s.ms + 600 >= since && s.ms <= g.ms + 300)
                .map(at)
                .collect();
            let one_way =
                window.windows(2).all(|w| w[1] >= w[0]) || window.windows(2).all(|w| w[1] <= w[0]);
            let moved = window.first() != window.last();
            if one_way && moved {
                turns.push((since, anchor, g.frame.col));
            }
        }
        (dir, anchor, since) = (d.signum(), g.frame.col, g.ms);
    }
    turns
}

/// THE PET NEVER TURNS BACK WHILE THE WRITING RUNS ONE WAY. Typed toward a
/// cat a little ahead of its stand (resting past the harness's mark on the
/// composer's upper rule, walking to that rest as the hand begins), the cat
/// waits for the writing and sets out with it.
#[test]
fn the_pet_never_turns_back_while_the_writing_runs_one_way() {
    let harness = Knobs {
        chips: true,
        brand: true,
        ..Knobs::SHIPPED
    };
    let chips = Knobs {
        chips: true,
        ..Knobs::SHIPPED
    };
    let runs: Vec<(&str, Knobs)> = [
        ("shipped", Knobs::SHIPPED),
        ("chips", chips),
        ("harness", harness),
    ]
    .into_iter()
    .flat_map(|(name, k)| {
        [(8u64, 0u64), (16, 0), (16, 150)]
            .into_iter()
            .map(move |(frame_ms, echo_ms)| {
                (
                    name,
                    Knobs {
                        frame_ms,
                        echo_ms,
                        ..k
                    },
                )
            })
    })
    .collect();
    let seen = crate::common::par_map(&runs, |&(name, knobs)| {
        let h = Host::replay(&TAKES[0], knobs);
        (name, knobs, turns_against_the_writing(&h.shots))
    });
    for (name, knobs, turns) in &seen {
        assert!(
            turns.is_empty(),
            "{name} at {} ms a frame, echo +{} ms: the cat turned back while the caret went one \
             way (ms, from col, to col): {turns:?}",
            knobs.frame_ms,
            knobs.echo_ms
        );
    }
}

/// A FLIGHT THAT LANDS INTO WRITING TOUCHES DOWN.
#[test]
fn a_flight_that_lands_into_writing_touches_down() {
    let harness = Knobs {
        chips: true,
        brand: true,
        ..Knobs::SHIPPED
    };
    let window = Take {
        name: "bracketed",
        src: "",
        rows: 40,
        cols: 120,
    };
    let mut runs: Vec<(&str, Knobs, Option<usize>, Option<SyntheticTake>)> = Vec::new();
    for (kn, k) in [("shipped", Knobs::SHIPPED), ("harness", harness)] {
        for take in 0..TAKES.len() {
            runs.push((kn, k, Some(take), None));
        }
        runs.push((kn, k, None, Some(bracketed_composer_take)));
        runs.push((kn, k, None, Some(full_transcript_composer_take)));
    }
    let seen = crate::common::par_map(&runs, |&(kn, knobs, take, events)| {
        let (name, h) = match (take, events) {
            (Some(t), _) => (TAKES[t].name, Host::replay(&TAKES[t], knobs)),
            (None, Some(e)) => (
                "windowed composer",
                Host::replay_events(&window, knobs, e()),
            ),
            _ => unreachable!("a take or a synthetic composer"),
        };
        let shots = &h.shots;
        let mut bad = Vec::new();
        for i in 1..shots.len() {
            let (prev, s) = (&shots[i - 1], &shots[i]);
            if !(prev.frame.action == PetAction::Leap && s.frame.action == PetAction::Land) {
                continue;
            }
            let mut keys = 0;
            let mut last = s.ms;
            for w in shots[..=i].windows(2).rev() {
                if w[1].caret == w[0].caret {
                    continue;
                }
                let forward = w[1].caret.0 == w[0].caret.0
                    && w[1].caret.1 > w[0].caret.1
                    && w[1].caret.1 - w[0].caret.1 <= 2;
                if !forward || last - w[1].ms > 600 {
                    break;
                }
                keys += 1;
                last = w[1].ms;
            }
            if keys < 3 {
                continue;
            }
            let end = shots[i..]
                .iter()
                .position(|x| x.frame.action != PetAction::Land)
                .map_or(shots.len() - 1, |k| i + k);
            let held = shots[end].ms - s.ms;
            let back = shots[i..end]
                .iter()
                .map(|x| s.frame.col - x.frame.col)
                .fold(0.0f32, f32::max);
            if held > 130 + 2 * knobs.frame_ms || back > 0.1 {
                bad.push((s.ms, held, back));
            }
        }
        (kn, name, bad)
    });
    for (kn, name, bad) in &seen {
        assert!(
            bad.is_empty(),
            "{name}, {kn}: landings into writing held or slid back (ms, ms held, cells back): \
             {bad:?}"
        );
    }
}
