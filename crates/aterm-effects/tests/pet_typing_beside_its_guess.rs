// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! The pet types beside its guess.

use std::sync::{Mutex, OnceLock, PoisonError};
use std::time::{Duration, Instant};

use aterm_core::render::RenderInput;
use aterm_core::terminal::{ClockReading, Terminal};
use aterm_effects::companion::{CompanionOwner, GlowOwnership, PetFacts};
use aterm_effects::cursor_glow::GlowStyle;
use aterm_effects::host::{FrameGeom, HostFrameInput, TerminalFacts};
use aterm_effects::kitty_pet::{PetAction, PetFrame, PetInputKind};
use aterm_effects::pet_world::{PetPane, PetRect, PetReserve, PetWorldFacts};
use aterm_effects::word_decorations::{DecoConfig, WordDecorations};
use aterm_lexicon::Lexicon;

use crate::common::{FrameHold, Rec, recording};

const CW: u16 = 15;
const CH: u16 = 28;
const MAX_SPEED: f32 = 26.0;
const TOP_SPEED: f32 = 48.0;
const FLICKER_MS: u64 = 350;
const GUESS_EXPIRY_MS: u64 = 250;

struct Take {
    name: &'static str,
    src: &'static str,
    rows: u16,
    cols: u16,
}

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
    /// The negative control: a protected surface (an overlay), which steps the
    /// pet aside and jumps it.
    Protected,
}

/// One replay's host: how it reserves the guess, its display, the program's
/// echo latency, and whether aterm's footer chips sit on the composer's rule.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
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
    /// whole run, and `pet_chrome` reserves that run whole, its strokes
    /// included: the host's line).
    brand: bool,
    /// The composer's upper rule reserved from end to end, its strokes
    /// included, with no harness on it: the host's line without its lockup
    /// and switch.
    rule_walled: bool,
    /// The composer's first line itself reserved from end to end: no stand
    /// on it for the escort either, so the cat rests under the composer,
    /// beside the chips, as `rule_walled` sent it until the escort passed
    /// under the chrome over its line.
    line_walled: bool,
    /// The host's chrome (the chips, the harness's lockup and switch) reserved only from
    /// this many ms into the take: aterm paints it once it has told the session is Claude
    /// Code's, a beat after the composer's first frame.
    chrome_from_ms: u64,
    /// The host's chrome as the shipped host hands it over: the cells its last finished pass
    /// painted (`aterm-gui`'s `PetChrome::held`), which that pass read off the frame before the
    /// one the pet looks at. The frame a composer moves in, the pet reads its new rows with the
    /// chrome still where the rows were; the next frame brings the chrome along.
    chrome_lag: bool,
    /// The window's presents, as a busy window draws them: each a vsync, two or three after
    /// the one before (one in five, one in twelve), late by up to 3 ms, from this seed
    /// ([`Host::next_present`]). 0: every vsync, on time.
    present_seed: u64,
    /// The host's footer facts: aterm's rewrite of Claude Code's rows under the composer's
    /// bottom rule (`claude_footer`'s `collect_mode_row`): the mode row's kept items laid flush
    /// at column 0 ([`Self::footer_words`] cells of them) and the rest blanked to the end of
    /// Claude's own words, and the row under it (where Claude's effort hint wraps) blanked
    /// whole. The pet reads Claude's cells, the frame aterm paints over after it looked:
    /// `pet_chrome` reserves the words aterm laid as protected and hands the cells it blanked
    /// as blank glass ([`PetReserve::blanked`]), Claude's letters under them hidden.
    footer_chrome: bool,
    /// [`Self::footer_chrome`] as the host handed it before it told the pet the glass it blanks
    /// (the negative control): the whole extent each rewrite wrote reserved as protected, each
    /// row up to Claude's last letter, and nothing blanked, so the pet read Claude's hidden
    /// letters as ink and the reserve as chrome.
    footer_whole: bool,
    /// The cells [`Self::footer_chrome`]'s rewrite lays its kept items in: [`MODE_ROW_WORDS`],
    /// or 0 for a mode row with no job or live state to keep, blanked whole.
    footer_words: u16,
    /// A chip of the host's on one row of the screen, `(row, col, cols)`, reserved as the
    /// footer's chips are (a Codex chip on the blank row over its status row); `cols` 0: none.
    row_chip: (u16, u16, u16),
}

impl Knobs {
    const SHIPPED: Self = Self {
        reserve: Reserve::Ink,
        frame_ms: 8,
        echo_ms: 0,
        chips: false,
        brand: false,
        rule_walled: false,
        line_walled: false,
        chrome_from_ms: 0,
        chrome_lag: false,
        present_seed: 0,
        footer_chrome: false,
        footer_whole: false,
        footer_words: MODE_ROW_WORDS,
        row_chip: (0, 0, 0),
    };
}

/// The cells aterm's rewrite of Claude Code's mode row lays its kept items in, flush at column 0
/// ([`Knobs::footer_chrome`]): `YOLO on · 1 shell`, as the owner's screenshot shows it.
const MODE_ROW_WORDS: u16 = 17;

const CHIPS_COLS: u16 = 57;

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
    /// The last column holding a letter on the row over the drawn body's feet row, when any:
    /// the head law slides a stand past them.
    letters_over: Option<u16>,
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
    /// The chrome the last present painted, the glass it blanked, and the content sequence of
    /// the frame it painted over, for [`Knobs::chrome_lag`].
    painted: (Vec<PetRect>, Vec<PetRect>, Option<u64>),
    /// The present schedule's state ([`Knobs::present_seed`]).
    schedule: u64,
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
            painted: (Vec::new(), Vec::new(), None),
            schedule: knobs.present_seed,
        }
    }

    /// The time to the next present ([`Knobs::present_seed`]): one vsync on a steady host.
    fn next_present(&mut self) -> Duration {
        let frame = Duration::from_millis(self.knobs.frame_ms);
        if self.schedule == 0 {
            return frame;
        }
        let mut x = self.schedule;
        x ^= x << 13;
        x ^= x >> 7;
        x ^= x << 17;
        self.schedule = x;
        let vsyncs = match x % 100 {
            0..8 => 3,
            8..28 => 2,
            _ => 1,
        };
        frame * vsyncs + Duration::from_micros((x >> 32) % 3_000)
    }

    fn ms(&self) -> u64 {
        self.now.saturating_duration_since(self.t0).as_millis() as u64
    }

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
        let chrome = now >= self.knobs.chrome_from_ms;
        let mut chips: Vec<PetRect> = rule
            .filter(|_| self.knobs.chips && chrome)
            .map(|row| {
                let w = f32::from(CHIPS_COLS);
                PetRect::new(row as f32, cols - 1.0 - w, 1.0, w)
            })
            .into_iter()
            .collect();
        if let Some((top, _)) = rules.filter(|_| self.knobs.brand && chrome) {
            let (mark, switch) = (f32::from(BRAND_MARK_COLS), f32::from(BRAND_SWITCH_COLS));
            chips.push(PetRect::new(top as f32, 2.0, 1.0, mark));
            chips.push(PetRect::new(top as f32, cols - 2.0 - switch, 1.0, switch));
            chips.push(PetRect::new(top as f32, 0.0, 1.0, cols));
        }
        let mut blanked: Vec<PetRect> = Vec::new();
        if let Some(bottom) = rule.filter(|_| self.knobs.footer_chrome && chrome) {
            for (row, words) in [(bottom + 1, self.knobs.footer_words), (bottom + 2, 0)] {
                let end = input
                    .cells
                    .get(row)
                    .and_then(|cells| cells.iter().rposition(|c| c.ch != ' ' && c.ch != '\0'));
                let Some(end) = end.map(|e| (e + 1) as f32) else {
                    continue;
                };
                let words = if self.knobs.footer_whole {
                    end
                } else {
                    f32::from(words).min(end)
                };
                if words > 0.0 {
                    chips.push(PetRect::new(row as f32, 0.0, 1.0, words));
                }
                if words < end {
                    blanked.push(PetRect::new(row as f32, words, 1.0, end - words));
                }
            }
        }
        if let (row, col, n @ 1..) = self.knobs.row_chip
            && chrome
        {
            chips.push(PetRect::new(
                f32::from(row),
                f32::from(col),
                1.0,
                f32::from(n),
            ));
        }
        if let Some((top, _)) = rules.filter(|_| self.knobs.rule_walled) {
            chips.push(PetRect::new(top as f32, 0.0, 1.0, cols));
        }
        if let Some((top, _)) = rules.filter(|_| self.knobs.line_walled) {
            chips.push(PetRect::new(top as f32 + 1.0, 0.0, 1.0, cols));
        }
        let mut painted_over = Some(input.content_seq);
        if self.knobs.chrome_lag {
            (chips, blanked, painted_over) =
                std::mem::replace(&mut self.painted, (chips, blanked, painted_over));
        }
        let reserve = match self.knobs.reserve {
            Reserve::Ink => PetReserve {
                protected: &chips,
                overlays: &[],
                ink: &typed,
                blanked: &blanked,
                painted_over,
            },
            Reserve::Protected => PetReserve {
                protected: &chips,
                overlays: &typed,
                ink: &[],
                blanked: &blanked,
                painted_over,
            },
        };
        self.owner
            .observe_console_reserved(input, &world, PetPane::full(input), reserve);
        let facts = TerminalFacts::read(&self.term, 7, false);
        let host = HostFrameInput::new(
            self.now,
            FrameGeom {
                rows: self.rows,
                cols: self.cols,
                cell_w: CW,
                cell_h: CH,
                origin_px: (0, 0),
            },
        );
        let tick = self.owner.prepare(
            PetFacts::new(
                &facts,
                &host,
                GlowOwnership {
                    enabled: true,
                    style: GlowStyle::RainbowKitty,
                    style_raw_names_pet: true,
                },
            ),
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
        let letters_over = (frame.row.round() as usize)
            .checked_sub(1)
            .and_then(|r| input.cells.get(r))
            .and_then(|row| row.iter().rposition(|c| c.ch.is_alphanumeric()))
            .map(|c| c as u16);
        self.shots.push(Shot {
            ms: self.ms(),
            frame,
            reason: self.owner.brain().console_reason(),
            guess,
            caret: (caret.row, caret.col),
            rule,
            letters_over,
        });
    }

    fn run_to(&mut self, ms: u64) {
        let t = self.t0 + Duration::from_millis(ms);
        while self.vsync <= t {
            self.now = self.vsync;
            self.present();
            let next = self.next_present();
            self.vsync += next;
        }
        self.now = self.now.max(t);
    }

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

/// The shots of `TAKES[take]` replayed under `knobs`, replayed once in the suite: every test
/// that reads that host reads the one replay (a replay is a function of its take and knobs).
fn replayed(take: usize, knobs: Knobs) -> &'static [Shot] {
    type Replays = Vec<((usize, Knobs), &'static OnceLock<Vec<Shot>>)>;
    static REPLAYS: Mutex<Replays> = Mutex::new(Vec::new());
    let cell = {
        let mut replays = REPLAYS.lock().unwrap_or_else(PoisonError::into_inner);
        if let Some(&(_, cell)) = replays.iter().find(|(key, _)| *key == (take, knobs)) {
            cell
        } else {
            let cell: &'static OnceLock<Vec<Shot>> = Box::leak(Box::new(OnceLock::new()));
            replays.push(((take, knobs), cell));
            cell
        }
    };
    cell.get_or_init(|| Host::replay(&TAKES[take], knobs).shots)
}

type SyntheticTake = fn() -> Vec<(u64, Rec)>;

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

fn bracketed_composer_take() -> Vec<(u64, Rec)> {
    bracketed_composer_typing(
        "in fact, i would like for the model use sigma section to be hidden by default but can be enabled. i do not really need to know that info at all, and the footer stays",
        0,
    )
}

fn bracketed_composer_typing(text: &str, rest: u64) -> Vec<(u64, Rec)> {
    bracketed_composer_bursts(&[(text, rest)])
}

/// [`bracketed_composer_typing`] in bursts: each burst's text typed a key every 85 ms onto the
/// draft so far, then its pause (ms) before the next.
fn bracketed_composer_bursts(bursts: &[(&str, u64)]) -> Vec<(u64, Rec)> {
    composer_bursts_under(
        &[
            " ▐▛███▜▌   Claude Code v2.1.305",
            "▝▜█████▛▘  Opus 5.5 · API Usage Billing",
            "  ▘▘ ▝▝    /private/tmp/lab/proj",
        ],
        bursts,
    )
}

/// [`bracketed_composer_bursts`] under the screen's first four rows `above` (a launch card, a
/// turn's spinner and tip) instead of the launch card.
fn composer_bursts_under(above: &[&str], bursts: &[(&str, u64)]) -> Vec<(u64, Rec)> {
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
    for (row, line) in above.iter().enumerate() {
        screen.push_str(&format!("\x1b[{};1H{line}", row + 1));
    }
    ev.push((0, Rec::Out(screen.into_bytes())));
    ev.push((10, Rec::Out(frame(""))));
    let mut t = 1_000;
    let mut draft = String::new();
    for &(text, rest) in bursts {
        for ch in text.chars() {
            ev.push((t, Rec::In(ch.to_string().into_bytes())));
            draft.push(ch);
            ev.push((t + 15, Rec::Out(frame(&draft))));
            t += 85;
        }
        if rest > 0 {
            t += rest;
            ev.push((t, Rec::Out(Vec::new())));
        }
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

fn standing(frame: &PetFrame) -> bool {
    use aterm_effects::pet_glyphs_gen::PetGlyphId;
    matches!(frame.pose, PetGlyphId::PetStand | PetGlyphId::PetStandEar)
}

const STRIDE_MS: f32 = 3.4 / MAX_SPEED * 1_000.0;

fn gait(frame: &PetFrame) -> Option<&'static str> {
    let id = aterm_effects::pet_glyphs_gen::PET_GLYPHS[frame.pose as usize].id;
    ["pet_walk_", "pet_lwalk_", "pet_run_"]
        .into_iter()
        .find(|family| id.starts_with(family))
}

const CREEP_SPEED: f32 = 3.0;
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
        (take, reserve, motion(replayed(take, knobs), knobs.frame_ms))
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
        (
            take,
            frame_ms,
            echo_ms,
            motion(replayed(take, knobs), frame_ms),
        )
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
        (
            take,
            frame_ms,
            echo_ms,
            motion(replayed(take, knobs), frame_ms),
        )
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
        // At 22 rows the wrap's growth never reaches the cat; a ride there is the seat under
        // the footer going down with the composer, judged with the rest above.
        if *rows != 22 {
            assert!(
                !m.rides.is_empty(),
                "{label}: the wrap grows the composer under the resting cat: rides {:?}",
                m.rides
            );
        }
    }
}

#[test]
fn the_pet_walks_the_composer_line_beside_its_caret_over_the_chips() {
    let knobs = Knobs {
        chips: true,
        ..Knobs::SHIPPED
    };
    let chips_from = TAKES[0].cols - 1 - CHIPS_COLS;
    let line: Vec<&Shot> = replayed(0, knobs)
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

const BODY_COLS_X10: u16 = 52;

const STAND_REACH: f32 = 12.0;

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
        let (cols, shots) = match events {
            None => (TAKES[0].cols, replayed(0, knobs).to_vec()),
            Some(events) => (
                window.cols,
                Host::replay_events(&window, knobs, events()).shots,
            ),
        };
        (name, cols, shots)
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
            let launched = launches[0].0;
            let landed = after
                .windows(2)
                .find(|w| {
                    w[0].ms >= launched
                        && w[0].frame.action == PetAction::Leap
                        && w[1].frame.action != PetAction::Leap
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
        let shots = match events {
            None => replayed(0, knobs).to_vec(),
            Some(events) => Host::replay_events(&window, knobs, events()).shots,
        };
        (name, shots)
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

/// THE HEAD LAW AT THE HOST'S LINE (2026-10-24, the owner: "the cursor kitty pet is drawn
/// BELOW the top line and I can't see it"): typed on past the harness's switch, the cat walks on
/// top of the composer's upper rule, the host's line, by the caret, whole and at full presence.
/// Until then it walked the composer's line under the switch, its head behind the switch's words
/// and the rule's strokes (1f8cc63681).
#[test]
fn the_pet_walks_on_top_of_the_composer_over_the_harness_switch() {
    let knobs = Knobs {
        chips: true,
        brand: true,
        ..Knobs::SHIPPED
    };
    let switch_from = TAKES[0].cols - 2 - BRAND_SWITCH_COLS;
    let line: Vec<&Shot> = replayed(0, knobs)
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
    // On top of the rule: the feet on the row over it, never lower (the body would reach into
    // the rule) and not a half row higher. By the caret: within STAND_REACH columns of it, or
    // where the head law stopped the stand, just past the letters over its row (a body there
    // keeps its head off them, so the veil never dims it).
    let on_top = |s: &Shot| {
        let over = f32::from(s.caret.0) - 2.0;
        s.frame.row <= over + 0.01 && s.frame.row > over - 0.5
    };
    let by_caret = |s: &Shot| {
        (s.frame.col - f32::from(s.caret.1)).abs() <= STAND_REACH
            || s.letters_over.is_some_and(|last| {
                s.frame.col > f32::from(s.caret.1)
                    && s.frame.col >= f32::from(last) + 1.0
                    && s.frame.col <= f32::from(last) + 1.1
            })
    };
    let off: Vec<_> = line
        .iter()
        .filter(|s| !on_top(s) || !by_caret(s) || s.frame.alpha < 255)
        .map(|s| {
            let f = s.frame;
            (s.ms, s.caret, f.col, f.row, f.alpha, s.letters_over)
        })
        .collect();
    assert!(
        off.is_empty(),
        "{} of {} presents off the top of the upper rule, away from the caret, or veiled \
         (ms, caret, col, row, alpha, last letter over its row): {:?}",
        off.len(),
        line.len(),
        &off[..off.len().min(6)]
    );
    let slid = line
        .iter()
        .filter(|s| (s.frame.col - f32::from(s.caret.1)).abs() > STAND_REACH)
        .count();
    assert!(
        slid < line.len() / 10,
        "the head law's slide past the letters is the exception: {slid} of {} presents",
        line.len()
    );
}

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

const POSE_MS: u64 = 25;

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
        (
            take,
            frame_ms,
            echo_ms,
            knobs.brand,
            pose_runs(replayed(take, knobs)),
        )
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
        (name, knobs, turns_against_the_writing(replayed(0, knobs)))
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
        let (name, shots) = match (take, events) {
            (Some(t), _) => (TAKES[t].name, replayed(t, knobs).to_vec()),
            (None, Some(e)) => (
                "windowed composer",
                Host::replay_events(&window, knobs, e()).shots,
            ),
            _ => unreachable!("a take or a synthetic composer"),
        };
        let shots = &shots;
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

/// One flight of a replay: `(ms it left, the caret then, the first frame on
/// the ground after it, the caret then)`.
type Flown = (u64, (u16, u16), PetFrame, (u16, u16));

/// Every flight of a replay ([`Flown`]).
fn flights_of(shots: &[Shot]) -> Vec<Flown> {
    shots
        .windows(2)
        .enumerate()
        .filter(|(_, w)| {
            w[1].frame.action == PetAction::Leap && w[0].frame.action != PetAction::Leap
        })
        .filter_map(|(i, w)| {
            let landed = shots[i + 1..]
                .iter()
                .find(|s| s.frame.action != PetAction::Leap)?;
            Some((w[1].ms, w[1].caret, landed.frame, landed.caret))
        })
        .collect()
}

/// The actions the cat shows on the ground before the flight that leaves at
/// `at`, back to the landing before it or to `since` ms, whichever came later, in
/// order, each run of one action once.
fn grounded_before(shots: &[Shot], since: u64, at: u64) -> Vec<PetAction> {
    let Some(off) = shots.iter().position(|s| s.ms == at) else {
        return Vec::new();
    };
    let mut seen: Vec<PetAction> = Vec::new();
    for s in shots[..off].iter().rev() {
        if s.ms < since || matches!(s.frame.action, PetAction::Land | PetAction::Leap) {
            break;
        }
        if seen.last() != Some(&s.frame.action) {
            seen.push(s.frame.action);
        }
    }
    seen.reverse();
    seen
}

/// One host's replay of the generating take: `(label, echo ms, harness, flights, the
/// actions on the ground before each since the turn end's walk began)`.
type HostFlights = (String, u64, bool, Vec<Flown>, Vec<Vec<PetAction>>);

/// The generating take at 120 and 60 Hz, echo as recorded and 150 ms late,
/// plain, with aterm's chips and with the harness's chrome ([`HostFlights`]).
fn generating_flights() -> Vec<HostFlights> {
    let runs: Vec<(u64, u64, &str, Knobs)> = [(8, 0), (16, 0), (8, 150), (16, 150)]
        .into_iter()
        .flat_map(|(frame_ms, echo_ms)| {
            [
                ("plain", Knobs::SHIPPED),
                (
                    "chips",
                    Knobs {
                        chips: true,
                        ..Knobs::SHIPPED
                    },
                ),
                (
                    "harness",
                    Knobs {
                        chips: true,
                        brand: true,
                        ..Knobs::SHIPPED
                    },
                ),
            ]
            .into_iter()
            .map(move |(host, k)| {
                (
                    frame_ms,
                    echo_ms,
                    host,
                    Knobs {
                        frame_ms,
                        echo_ms,
                        ..k
                    },
                )
            })
        })
        .collect();
    crate::common::par_map(&runs, |&(frame_ms, echo_ms, host, knobs)| {
        let shots = replayed(0, knobs);
        let flights = flights_of(shots);
        // The turn end's walk: the composer's first move to another row from 32 s on.
        let walk = shots
            .windows(2)
            .find(|w| w[1].ms >= 32_000 + echo_ms && w[1].caret.0 != w[0].caret.0)
            .map_or(0, |w| w[1].ms);
        let grounded = flights
            .iter()
            .map(|&(at, ..)| grounded_before(shots, walk, at))
            .collect();
        (
            format!("{frame_ms} ms a frame, echo +{echo_ms} ms, {host}"),
            echo_ms,
            knobs.brand,
            flights,
            grounded,
        )
    })
}

/// A REPAINT IS ONE FLIGHT, TO WHERE THE CARET SETTLES. At 32 s into the
/// generating take Claude Code submits the prompts queued behind its turn and
/// lays its transcript out again, a DEC 2026 frame every 50 ms or so: its
/// composer goes down the screen from row 22 to row 36 through six places in
/// 424 ms, no key behind any of them. The cat waits the walk out in the
/// crouch it gathered at the first move and bounds once, out of that crouch,
/// onto the row where the composer stops, at 120 and 60 Hz, echo as recorded
/// and 150 ms late, with and without the host's chrome. With the harness on,
/// whose top rule the host holds whole, that row is the top of the composer's
/// upper rule, over the caret's line (aterm-effects SPECS
/// `host-line-head-law`): on the caret's line its head would be under the rule.
///
/// NEGATIVE CONTROL (as run on main before the fix): 2 flights at
/// 60 Hz and 3 at 120 Hz in every host, the first landing on row 20 or 25
/// with the caret on row 36; the 120 Hz second was a hop off the transcript's
/// new words from where the first came down. With the one flight but the
/// coil stood up when the walk armed the stream watch, the cat crouched,
/// perked, sat and sprang from the sit: `[Crouch, Perk, Sit]` before the
/// bound in every host. (Read from the walk's first move: what the cat did on
/// the ground before it, beside the writing the wrap's bound brought it down by,
/// is the writing's.)
#[test]
fn a_repaint_that_walks_the_caret_is_one_flight_to_where_it_settles() {
    for (label, echo, harness, flights, grounded) in generating_flights() {
        let turn_end: Vec<_> = flights
            .iter()
            .zip(&grounded)
            .filter(|((at, ..), _)| (32_000 + echo..=34_500 + echo).contains(at))
            .map(|(f, g)| {
                assert_eq!(
                    g.as_slice(),
                    [PetAction::Crouch],
                    "{label}: on the ground before the turn's bound at {} ms",
                    f.0
                );
                f
            })
            .collect();
        let seen: Vec<_> = turn_end
            .iter()
            .map(|(at, from, landed, to)| (*at, *from, (landed.col.round(), landed.row), *to))
            .collect();
        assert_eq!(
            turn_end.len(),
            1,
            "{label}: the turn's end, flights (ms, caret, landing at (col, row), caret): {seen:?}"
        );
        let (_, _, landed, caret) = turn_end[0];
        // The caret settles on the composer's first line, under its upper rule.
        let row = if harness {
            f32::from(caret.0) - 2.0
        } else {
            f32::from(caret.0)
        };
        assert!(
            (landed.row - row).abs() < 0.5,
            "{label}: the bound came down on row {} with the caret on row {} (row {row} wanted)",
            landed.row,
            caret.0
        );
    }
}

/// THE FIRST SCREENS ARE NO STREAM TO WATCH. Claude Code's first screens (its
/// trust prompt, its launch card, the prompt submitted) walk the caret over
/// six rows in 400 ms, which arms the stream watch; the watch's live-edge hug
/// then took the cat to the newest inked row, the spinner's, two rows under a
/// caret that never went there, and its eviction off the launch card's words
/// ran it along them. Each flight of the session's first seconds lands where
/// the caret was as it left or as it came down, plain and with the chips. (With
/// the harness the host holds the composer's upper rule whole and no stand is
/// under it; that session's one trip, to where the cat rests, is
/// `the_sessions_first_trip_is_its_only_one`'s.)
///
/// NEGATIVE CONTROL (as run on main before the fix): at 120 Hz the
/// flight at 1.57 s to row 14 with the caret on row 12; with the echo 150 ms
/// late, the hop at 0.39 s along row 0 with the caret on rows 6 and 12.
#[test]
fn the_first_screens_send_the_cat_only_where_the_caret_was() {
    for (label, echo, _, flights, _) in generating_flights()
        .into_iter()
        .filter(|(_, _, harness, ..)| !harness)
    {
        for (at, from, landed, to) in flights.iter().filter(|(at, ..)| *at <= 2_500 + echo) {
            assert!(
                [from.0, to.0]
                    .iter()
                    .any(|&r| (landed.row - f32::from(r)).abs() < 0.5),
                "{label}: the flight at {at} ms came down on row {} with the caret on row {} as it \
                 left and {} as it landed",
                landed.row,
                from.0,
                to.0
            );
        }
    }
}

/// A FLIGHT COMES DOWN WHERE THE CARET WENT. Claude Code moves its prompt down
/// the screen as its first screens and a submitted prompt lay the transcript
/// out, a move every few hundred milliseconds, no key behind any of them. A
/// flight in the air when the prompt moves rows goes on to its new row, drawn
/// out if it must be to stay inside the animal's speed (the session's first
/// bound, to row 10 as the prompt went to row 12, the echo as recorded or
/// 150 ms late); a cat on the ground the prompt moves under rides down with
/// the rows round it. Each flight of the session's first 13 s lands on the
/// caret's row as it comes down, at 120 and 60 Hz, the echo as recorded and
/// 150 ms late, plain and with the chips. (With the harness no stand is under
/// the composer's upper rule the host holds whole; that session's one trip is
/// `the_sessions_first_trip_is_its_only_one`'s.)
///
/// NEGATIVE CONTROL (as run on main before the re-timing, b81b2c2ff0 +
/// merges): with the echo 150 ms late the first bound came down on row 10
/// with the caret on row 12.
#[test]
fn a_flight_comes_down_on_the_row_the_caret_went_to() {
    for (label, _, _, flights, _) in generating_flights()
        .into_iter()
        .filter(|(_, _, harness, ..)| !harness)
    {
        let early: Vec<_> = flights.iter().filter(|(at, ..)| *at <= 13_000).collect();
        assert!(
            early.len() >= 2,
            "{label}: fixture: the first screens and the submit fly ({early:?})"
        );
        for (at, _, landed, to) in early {
            assert!(
                (landed.row - f32::from(to.0)).abs() < 0.5,
                "{label}: the flight at {at} ms came down on row {} with the caret on row {}",
                landed.row,
                to.0
            );
        }
    }
}

/// A MID-LINE INSERT WRAPS IN ONE FLIGHT AT MOST. The owner's long insert types
/// into the last line of Claude Code's composer, the rest of the line pushed on
/// ahead of the caret to the wall, the lines above it lettered to the margin.
/// The cat follows the writing from a seat its head stays clear on: the
/// composer's footer under the bottom rule, or the line under the caret's. As
/// the caret comes to the margin the pushed word wraps down and the caret's row
/// clears; brought up onto it a second before the wrap, the cat hopped there and
/// bounded back down at the wrap. It waits where it is now: on the line the
/// caret wraps onto or on a seat under it, and any flight near the wrap lands
/// there too, one at most, never on the row the caret left, at 120 and 60 Hz,
/// echo as recorded and 150 ms late, with and without the host's chrome.
///
/// NEGATIVE CONTROL (as run on main before the fix): 2 flights within 1.5 s of
/// the wrap in every host, the first a hop up onto row 56 at col 139 with the
/// caret at col 135.
#[test]
fn a_mid_line_insert_wraps_in_one_flight() {
    let runs: Vec<(u64, u64, bool)> = [(8, 0), (16, 0), (8, 150), (16, 150)]
        .into_iter()
        .flat_map(|(f, e)| [(f, e, false), (f, e, true)])
        .collect();
    let seen = crate::common::par_map(&runs, |&(frame_ms, echo_ms, harness)| {
        let knobs = Knobs {
            frame_ms,
            echo_ms,
            chips: harness,
            brand: harness,
            ..Knobs::SHIPPED
        };
        let shots = replayed(3, knobs);
        let cols = TAKES[3].cols;
        // Each wrap: when, the line the caret wrapped onto, and the cat's row then.
        let wraps: Vec<(u64, u16, f32)> = shots
            .windows(2)
            .filter(|w| {
                w[1].caret.0 == w[0].caret.0 + 1 && w[0].caret.1 + 10 >= cols && w[1].caret.1 < 10
            })
            .map(|w| (w[1].ms, w[1].caret.0, w[0].frame.row))
            .collect();
        (frame_ms, echo_ms, harness, wraps, flights_of(shots))
    });
    for (frame_ms, echo_ms, harness, wraps, flights) in &seen {
        let label = format!(
            "{frame_ms} ms a frame, echo +{echo_ms} ms{}",
            if *harness { ", harness" } else { "" }
        );
        assert_eq!(wraps.len(), 1, "{label}: the insert wraps once: {wraps:?}");
        let (at, line, cat_row) = wraps[0];
        let line = f32::from(line);
        assert!(
            cat_row > line - 0.5,
            "{label}: the cat waits on the line the caret wraps onto (row {line}) or under it, \
             at row {cat_row}"
        );
        let near: Vec<_> = flights
            .iter()
            .filter(|(ms, ..)| *ms + 1_500 >= at && *ms <= at + 1_500)
            .map(|(ms, from, landed, to)| (*ms, *from, (landed.col.round(), landed.row), *to))
            .collect();
        assert!(
            near.len() <= 1 && near.iter().all(|(.., (_, row), _)| *row > line - 0.5),
            "{label}: flights within 1.5 s of the wrap at {at} ms onto row {line}, one at most \
             and onto that row or under it (ms, caret, landing at (col, row), caret): {near:?}"
        );
    }
}
/// The frames of `shots` between `from` and `to` ms with the cat on glass: how far the
/// cat is from the caret's cell, `(ms, cells)`, rows counted as two columns, measured to
/// the body's nearest edge (`art_cols` wide).
fn distances_from_the_caret(shots: &[Shot], from: u64, to: u64) -> Vec<(u64, f32)> {
    let width = aterm_effects::kitty_pet::art_cols(CW, CH);
    shots
        .iter()
        .filter(|s| (from..=to).contains(&s.ms) && s.frame.alpha > 0)
        .map(|s| {
            let cc = f32::from(s.caret.1);
            let dc = if cc < s.frame.col {
                s.frame.col - cc
            } else {
                (cc - s.frame.col - width).max(0.0)
            };
            let dr = s.frame.row - f32::from(s.caret.0);
            (s.ms, dc.hypot(2.0 * dr))
        })
        .collect()
}

/// THE WRITING IS FOLLOWED, NOT WATCHED FROM THE FAR MARGIN. The owner's long
/// insert types for ten seconds into the middle of the last line of Claude
/// Code's composer, every line above lettered to within a few cells of the
/// margin, the rest of the caret's line pushed on ahead of it. The stand beside
/// the caret is on those letters; the head law (a cat's head never on a letter) took the cat
/// along the row to the only spot whose row above held no letter, the far
/// margin, 80 to 125 cells from the writing for the whole of it. It sits on the
/// composer's footer under the caret now (its head over the bottom rule, no
/// letter under it), or beside the chips that cover the footer's end, and goes
/// with the writing. It sets out once the arrow keys have left the caret at
/// the insert's point half a second (`HEAD_SEAT_SETTLE`: a caret walked to and
/// fro by words is watched from where the cat stands), and its two bounds
/// across the composer have come down by the insert's first second: from then
/// on never more than 30 cells from the caret (rows counted as two) while the
/// insert is typed, at 120 and 60 Hz, the echo as recorded and 150 ms late,
/// plain, with the chips and with the harness.
///
/// NEGATIVE CONTROL (as run on main before the fix): 123 cells at most and 79
/// at the median in every host, every frame further than 30.
#[test]
fn a_long_insert_is_followed_from_near_the_writing() {
    let runs: Vec<(u64, u64, &str, Knobs)> = [(8, 0), (16, 0), (8, 150), (16, 150)]
        .into_iter()
        .flat_map(|(frame_ms, echo_ms)| {
            [
                ("plain", Knobs::SHIPPED),
                (
                    "chips",
                    Knobs {
                        chips: true,
                        ..Knobs::SHIPPED
                    },
                ),
                (
                    "harness",
                    Knobs {
                        chips: true,
                        brand: true,
                        ..Knobs::SHIPPED
                    },
                ),
            ]
            .into_iter()
            .map(move |(host, k)| {
                (
                    frame_ms,
                    echo_ms,
                    host,
                    Knobs {
                        frame_ms,
                        echo_ms,
                        ..k
                    },
                )
            })
        })
        .collect();
    let seen = crate::common::par_map(&runs, |&(frame_ms, echo_ms, host, knobs)| {
        let far = distances_from_the_caret(replayed(3, knobs), 13_200 + echo_ms, 22_300 + echo_ms)
            .into_iter()
            .filter(|&(_, d)| d > 30.0)
            .collect::<Vec<_>>();
        (
            format!("{frame_ms} ms a frame, echo +{echo_ms} ms, {host}"),
            far,
        )
    });
    for (label, far) in seen {
        assert!(
            far.is_empty(),
            "{label}: {} frames of the insert with the cat over 30 cells from the caret (ms, \
             cells): {:?}",
            far.len(),
            &far[..far.len().min(6)]
        );
    }
}

/// The long insert's events with each multi-byte printable input split into one key a
/// millisecond, as the app notes each key it sends (the owner's dictation types its chunks
/// as keys).
fn long_insert_by_keys() -> Vec<(u64, Rec)> {
    let mut out = Vec::new();
    for (ms, ev) in recording(TAKES[3].src, 1_000_000) {
        match ev {
            Rec::In(b) if b.as_slice() == b"\x1b[99;5u" => {
                out.push((ms, Rec::In(b)));
                break;
            }
            Rec::In(b) if b.len() > 1 && b.iter().all(|c| (0x20..0x7f).contains(c)) => {
                for (i, c) in b.iter().enumerate() {
                    out.push((ms + i as u64 / 8, Rec::In(vec![*c])));
                }
            }
            ev => out.push((ms, ev)),
        }
    }
    out
}

/// THE ARROW KEYS WALK THE CARET, NOT THE CAT TO THE FAR MARGIN. After the
/// long insert's dictation the cat stands beside the line's end; the arrow
/// keys then walk the caret back into the line, letter by letter, to the
/// insert's point. While the caret is on the move the head law's slide along
/// the row stays within reach of the caret (`HEAD_SEAT_REACH`), and the seat
/// under the box waits for the caret to rest: no landing past column 100 from
/// the dictation's end to the first key of the insert, and two flights at
/// most, at 120 and 60 Hz, plain and with the harness, the dictation typed as
/// keys.
///
/// NEGATIVE CONTROL (as run before the fix): the cat flew from the line's end
/// to column 137 as the arrows walked the caret, and crossed back in two
/// bounds once they stopped.
#[test]
fn the_arrow_keys_walk_the_caret_not_the_cat_to_the_far_margin() {
    for (frame_ms, brand) in [(8u64, false), (16, false), (8, true), (16, true)] {
        let knobs = Knobs {
            frame_ms,
            chips: brand,
            brand,
            ..Knobs::SHIPPED
        };
        let h = Host::replay_events(&TAKES[3], knobs, long_insert_by_keys());
        let walk: Vec<_> = flights_of(&h.shots)
            .into_iter()
            .filter(|(at, ..)| (7_500..12_180).contains(at))
            .map(|(at, from, landed, to)| (at, from, (landed.col.round(), landed.row), to))
            .collect();
        assert!(
            walk.len() <= 2 && walk.iter().all(|(_, _, (col, _), _)| *col <= 100.0),
            "{frame_ms} ms a frame, harness {brand}: flights from the dictation's end to the \
             insert (ms, caret, landing at (col, row), caret): {walk:?}"
        );
    }
}

/// THE COMPOSER'S RULE CARRIES THE CAT IT GROWS UNDER. With the harness the
/// cat waits on top of the composer's upper rule (the host's line) as the
/// owner's dictation types the long insert's lines; the composer grows a row
/// up under it with each, and where the rows round the cat rose with the rule
/// the host carries the cat up with them (`line_ride`). Hopped off as chrome
/// come over it instead, it was a hop for every row. The dictation's rows carry
/// the cat on at least four presents, at 120 and 60 Hz.
///
/// NEGATIVE CONTROL (as run before the fix): no present carried; nine hops.
#[test]
fn the_composers_rule_carries_the_cat_it_grows_under() {
    for frame_ms in [8u64, 16] {
        let knobs = Knobs {
            frame_ms,
            chips: true,
            brand: true,
            ..Knobs::SHIPPED
        };
        let h = Host::replay_events(&TAKES[3], knobs, long_insert_by_keys());
        let carried = h
            .shots
            .iter()
            .filter(|s| (2_300..7_700).contains(&s.ms) && s.reason == "content-ride")
            .count();
        assert!(
            carried >= 4,
            "{frame_ms} ms a frame: the dictation's growing composer carried the cat on \
             {carried} presents"
        );
    }
}

/// THE SESSION'S FIRST TRIP IS ITS ONLY ONE. A cat asleep at the top-left wakes
/// as Claude Code's first screens lay out the session and its first turn pushes
/// the composer down the screen. It goes once, and that trip ends where it then
/// rests, by the prompt: plain, on the composer's line, where the first key will
/// be typed (a prompt that moves on while the cat is in the air takes the landing
/// with it; a prompt pushed down under the resting cat carries it down). With the
/// harness, whose upper rule the host holds whole (aterm-effects SPECS
/// `host-line-head-law`: no stand under it), the console seats a resting cat on
/// the clean ground nearest the caret's stand (here under the composer's footer:
/// the first turn's spinner is over the rule), and a seat taken for the first
/// composer goes with the caret when the turn pushes it down: the landing goes on
/// to the new seat, and the cat rests within four rows of the caret's.
/// Judged on the first 10 s at 120 and 60 Hz, with the echo from as recorded to
/// 300 ms late, plain and with the harness: one flight at most, landing where the
/// cat is at 3 s; plain, on the caret's row; with the harness, by the composer.
///
/// NEGATIVE CONTROL (as run before the fix, a seat kept anywhere in reach): with
/// the harness and the echo 10 ms late or more the cat rested at row 3.85 under
/// the launch card, eight rows from the caret's row 12, until the first turn's end
/// at 11.7 s sent it down.
#[test]
fn the_sessions_first_trip_is_its_only_one() {
    let mut runs: Vec<(String, Knobs)> = Vec::new();
    for frame_ms in [8u64, 16] {
        for echo_ms in [0u64, 10, 20, 40, 80, 120, 150, 200, 300] {
            for (host, k) in [
                ("plain", Knobs::SHIPPED),
                (
                    "harness",
                    Knobs {
                        chips: true,
                        brand: true,
                        ..Knobs::SHIPPED
                    },
                ),
            ] {
                runs.push((
                    format!("{frame_ms} ms a frame, echo +{echo_ms} ms, {host}"),
                    Knobs {
                        frame_ms,
                        echo_ms,
                        ..k
                    },
                ));
            }
        }
    }
    let seen = crate::common::par_map(&runs, |(label, knobs)| {
        let h = Host::replay(&TAKES[0], *knobs);
        let flights: Vec<_> = flights_of(&h.shots)
            .into_iter()
            .filter(|(at, ..)| *at < 10_000)
            .map(|(at, from, landed, to)| (at, from, (landed.col.round(), landed.row), to))
            .collect();
        let at3 = h
            .shots
            .iter()
            .find(|s| s.ms >= 3_000 + knobs.echo_ms)
            .map(|s| (s.frame.col, s.frame.row, s.caret));
        (label.clone(), knobs.brand, flights, at3)
    });
    for (label, harness, flights, at3) in seen {
        assert!(
            flights.len() <= 1,
            "{label}: the session's first trips (ms, caret, landing at (col, row), caret): \
             {flights:?}"
        );
        let (col, row, caret) = at3.expect("frames at 3 s");
        if harness {
            for (at, _, (_, landed), _) in &flights {
                assert!(
                    (row - landed).abs() < 0.5,
                    "{label}: the flight at {at} ms came down on row {landed}, and at 3 s the \
                     cat rests at ({col:.1}, {row})"
                );
            }
            assert!(
                (row - f32::from(caret.0)).abs() <= 4.0,
                "{label}: at 3 s the cat rests at ({col:.1}, {row}), the caret at {caret:?}: \
                 not by the composer"
            );
        } else {
            assert!(
                (row - f32::from(caret.0)).abs() < 0.5,
                "{label}: at 3 s the cat rests at ({col:.1}, {row}), the caret at {caret:?}"
            );
        }
    }
}

/// ONE SEAT FOR THE WHOLE LINE. In the long insert the cat is seated on the composer's footer,
/// under the line the caret inserts into, from 10.6 s. With aterm's chips on the lower rule the
/// footer has no head room under them, so the seat stops at their left edge as the caret runs on
/// past it; the insert pushes its tail onto a new line (22.7 s), which opens room under the
/// caret and beside it at the line's end. The cat keeps its seat on the footer for the whole
/// line, and the wrap (27.3 s) is the line's only flight, as it is without the chips. Taken up
/// to the room the growth opened, it flew there and back down at the wrap. Chips and harness,
/// at 120 and 60 Hz, the echo as recorded and 150 ms late: one flight from 11 s to 29.5 s.
///
/// NEGATIVE CONTROL (as run before the fix): two, at 22.68 s up to row 57 and 27.34 s down to
/// the footer (chips, 120 Hz).
#[test]
fn a_long_line_by_the_chips_costs_only_its_wrap() {
    for brand in [false, true] {
        for (frame_ms, echo_ms) in [(8u64, 0u64), (16, 0), (8, 150), (16, 150)] {
            let knobs = Knobs {
                frame_ms,
                echo_ms,
                chips: true,
                brand,
                ..Knobs::SHIPPED
            };
            let span = (11_000 + echo_ms)..(29_500 + echo_ms);
            let flew: Vec<_> = flights_of(replayed(3, knobs))
                .into_iter()
                .filter(|(at, ..)| span.contains(at))
                .map(|(at, from, landed, to)| (at, from, (landed.col.round(), landed.row), to))
                .collect();
            assert!(
                flew.len() == 1,
                "harness {brand}, {frame_ms} ms a frame, echo +{echo_ms} ms: flights (ms, caret, \
                 landing, caret) {flew:?}"
            );
        }
    }
}

/// THE ESCORT'S TAIL SENDS NO ONE OUT. In the generating take the cat sits watching three
/// deletions walk the caret back three cells (35.4 s to 37.1 s) and stands up 1.1 s after the
/// last, with 0.3 s of the escort left. Its station is the rest's by then: it sets out along
/// the top of the composer for the clean seat past the transcript's last line, and never toward
/// the stand over the caret. Sent to that stand, it walked 1.8 cells left and turned back as the
/// rest took over, with the caret still. At 120 and 60 Hz, the echo as recorded and 150 ms
/// late, with the harness: from 37.9 s to 39.7 s the cat never goes back 0.5 cells from where
/// it set out.
///
/// NEGATIVE CONTROL (as run before the fix): 1.8 cells back (6.7 to 4.9) at 120 Hz.
#[test]
fn the_escorts_tail_sends_no_one_back_to_its_stand() {
    for (frame_ms, echo_ms) in [(8u64, 0u64), (16, 0), (8, 150), (16, 150)] {
        let knobs = Knobs {
            frame_ms,
            echo_ms,
            chips: true,
            brand: true,
            ..Knobs::SHIPPED
        };
        let span = (37_900 + echo_ms)..(39_700 + echo_ms);
        let cols: Vec<f32> = replayed(0, knobs)
            .iter()
            .filter(|s| span.contains(&s.ms) && s.frame.alpha > 0)
            .filter(|s| s.frame.action != PetAction::Leap)
            .map(|s| s.frame.col)
            .collect();
        let start = cols.first().copied().unwrap_or(0.0);
        let least = cols.iter().copied().fold(start, f32::min);
        assert!(
            start - least < 0.5,
            "{frame_ms} ms a frame, echo +{echo_ms} ms: back {:.2} cells from {start:.2}",
            start - least
        );
    }
}

/// THE WRITING BRINGS THE STAND. After the long insert's wrap the cat lands half a cell past
/// its stand on the footer, a stand the footer's words hold still while the caret runs toward
/// it: the cat holds there and the stand comes to it. Eased back the half cell, it walked on
/// with the caret a second later (a turn-back no caret move made). Plain, at 120 and 60 Hz, the
/// echo as recorded and 150 ms late: from the wrap's landing for two seconds the cat never
/// goes back 0.3 cells from where it came down.
///
/// NEGATIVE CONTROL (as run before the fix): 0.50 cells back (38.58 to 38.08) at 60 Hz with
/// the echo 150 ms late.
#[test]
fn a_cat_ahead_of_the_writing_holds_where_it_came_down() {
    for (frame_ms, echo_ms) in [(8u64, 0u64), (16, 0), (8, 150), (16, 150)] {
        let knobs = Knobs {
            frame_ms,
            echo_ms,
            ..Knobs::SHIPPED
        };
        let shots = replayed(3, knobs);
        let wrap = flights_of(shots)
            .into_iter()
            .filter(|(at, ..)| *at >= 26_000 + echo_ms)
            .map(|(_, _, landed, _)| landed)
            .next()
            .expect("fixture: the wrap's flight");
        let landed_at = shots
            .iter()
            .position(|s| s.frame.action == PetAction::Land && s.frame.col == wrap.col)
            .expect("fixture: its landing");
        let from = shots[landed_at].ms;
        let back = shots[landed_at..]
            .iter()
            .take_while(|s| s.ms <= from + 2_000)
            .filter(|s| s.frame.alpha > 0 && s.frame.action != PetAction::Leap)
            .map(|s| wrap.col - s.frame.col)
            .fold(0.0f32, f32::max);
        assert!(
            back < 0.3,
            "{frame_ms} ms a frame, echo +{echo_ms} ms: back {back:.2} cells from the landing \
             at {:.2} ({from} ms)",
            wrap.col
        );
    }
}

/// A SEAT LOST TO THE WORDS IS LEFT ON FOOT. As the generating take's third turn begins
/// (39.75 s) the cat's seat on top of the composer is covered by the turn's tip, and it flies to
/// the clean ground beside the spinner's new line; at 40.9 s the line counts its first second
/// over that seat. The cat walks along its own row past the words, and rests there until typing
/// begins (46.3 s). Flown a row up instead, the turn cost two flights in 0.8 s. At 120 and 60 Hz,
/// the echo as recorded and 150 ms late, with the harness: no flight between 40.5 s and 46 s.
///
/// NEGATIVE CONTROL (as run before the fix): a flight at 40.91 s from (12.4, 31.85) to
/// (3.4, 30.85).
#[test]
fn a_seat_a_status_line_counts_over_is_left_on_foot() {
    for (frame_ms, echo_ms) in [(8u64, 0u64), (16, 0), (8, 150), (16, 150)] {
        let knobs = Knobs {
            frame_ms,
            echo_ms,
            chips: true,
            brand: true,
            ..Knobs::SHIPPED
        };
        let span = (40_500 + echo_ms)..(46_000 + echo_ms);
        let flew: Vec<_> = flights_of(replayed(0, knobs))
            .into_iter()
            .filter(|(at, ..)| span.contains(at))
            .map(|(at, from, landed, to)| (at, from, (landed.col, landed.row), to))
            .collect();
        assert!(
            flew.is_empty(),
            "{frame_ms} ms a frame, echo +{echo_ms} ms: flights (ms, caret, landing, caret) {flew:?}"
        );
    }
}

/// THE REST WHERE THE TYPING WALKS. With the harness on, the escort walks the top of the
/// composer over the caret, and a cat at rest is seated there too, past the launch card's last
/// line, where the top of the composer is clear: each pause in the writing sends it along the
/// line to that seat, and each new burst finds it there, waiting for the caret or walking with
/// it. Seated under the footer instead, it flew down at each pause and up at each burst. Three
/// bursts with three-second pauses, at 120 and 60 Hz, the chrome as the host hands it a frame
/// late: after the first burst's flight up onto the line, no flight.
///
/// NEGATIVE CONTROL (as run before the fix): a flight down under the footer at the first pause
/// (3.7 s) and back up at the second burst (5.4 s).
#[test]
fn bursts_of_typing_by_a_hosted_composer_cost_no_flight_after_the_first() {
    let window = Take {
        name: "bursts",
        src: "",
        rows: 40,
        cols: 120,
    };
    for frame_ms in [8u64, 16] {
        let knobs = Knobs {
            frame_ms,
            chips: true,
            brand: true,
            chrome_lag: true,
            ..Knobs::SHIPPED
        };
        let h = Host::replay_events(
            &window,
            knobs,
            bracketed_composer_bursts(&[
                ("in fact, i would", 3_000),
                (" like the model", 3_000),
                (" section hidden by default", 3_000),
            ]),
        );
        let flights = flights_of(&h.shots);
        let first_burst = flights
            .iter()
            .position(|(at, ..)| *at >= 1_000)
            .unwrap_or(flights.len());
        let after: Vec<_> = flights
            .iter()
            .skip(first_burst + 1)
            .map(|(at, from, landed, to)| (*at, *from, (landed.col.round(), landed.row), *to))
            .collect();
        assert!(
            after.is_empty(),
            "{frame_ms} ms a frame: flights after the first burst's, (ms, caret, landing, caret) \
             {after:?}"
        );
    }
}

/// THE LINE HOLDS THROUGH A PAUSE. Claude Code's turn lays its tip over the harness composer,
/// two rows over its top rule, and the tip runs nearly the width of the pane: the top of the
/// composer, the escort's ground over the caret, has no clean seat along it. Typed in bursts with
/// two-second pauses, the cat goes up onto the line with the first burst and holds there through
/// each pause, veiled over the tip, and walks on with the next burst. Flown under the footer as
/// the escort ended, it flew back up at the next key: two flights each pause (the harness's
/// first turn, 13.1 s and 13.9 s). A long pause still seats it clean, clear of every word: one
/// flight once the hold runs out, and it rests whole. At 120 and 60 Hz, the chrome in time and a
/// frame late.
///
/// NEGATIVE CONTROL (as run before the fix): flights down under the footer at each pause (3.7 s,
/// 7.0 s) and up again at each burst (4.4 s, 7.6 s).
#[test]
fn a_pause_in_the_writing_costs_no_trip_off_the_line() {
    let window = Take {
        name: "tipped bursts",
        src: "",
        rows: 40,
        cols: 120,
    };
    let tip: String =
        "  \u{23bf}  Tip: Start with small features or bug fixes, tell Claude to propose \
                       a plan, and verify its suggested edits before you accept them"
            .chars()
            .take(118)
            .collect();
    let events = || {
        composer_bursts_under(
            &[
                "\u{273b} Churning\u{2026} (3s \u{b7} \u{2193} 12 tokens)",
                "",
                &tip,
                "",
            ],
            &[
                ("in fact, i would", 2_000),
                (" like the model", 2_000),
                (" section hidden", 8_000),
            ],
        )
    };
    let last_key = events()
        .iter()
        .filter(|(_, ev)| matches!(ev, Rec::In(_)))
        .map(|(ms, _)| *ms)
        .max()
        .expect("fixture: keys");
    for frame_ms in [8u64, 16] {
        for chrome_lag in [false, true] {
            let knobs = Knobs {
                frame_ms,
                chips: true,
                brand: true,
                chrome_lag,
                ..Knobs::SHIPPED
            };
            let h = Host::replay_events(&window, knobs, events());
            let flights: Vec<_> = flights_of(&h.shots)
                .into_iter()
                .filter(|(at, ..)| *at >= 1_000)
                .map(|(at, from, landed, to)| (at, from, (landed.col.round(), landed.row), to))
                .collect();
            let writing: Vec<_> = flights.iter().filter(|f| f.0 <= last_key).collect();
            assert!(
                writing.len() == 1,
                "{frame_ms} ms a frame, chrome late {chrome_lag}: flights while the writing \
                 pauses and goes on, (ms, caret, landing, caret) {writing:?}"
            );
            let rested: Vec<_> = flights.iter().filter(|f| f.0 > last_key).collect();
            let end = h.shots.last().expect("fixture: shots").frame;
            assert!(
                rested.len() == 1 && end.alpha == 255,
                "{frame_ms} ms a frame, chrome late {chrome_lag}: after the long pause, flights \
                 {rested:?}, resting at ({:.1}, {:.2}) alpha {}",
                end.col,
                end.row,
                end.alpha
            );
        }
    }
}

/// THE HOST'S CHROME RIDES ITS ROWS. The shipped host paints its chrome (the harness's line
/// over the composer's upper rule, the chips on its lower rule) after the pet has looked at a
/// frame, so the pet reads the chrome its last pass laid over the frame before
/// ([`Knobs::chrome_lag`]). The frame Claude Code moves its composer in, that chrome lies where the
/// rows were: in a window, a cat that rode down with the composer's rule was under the host's
/// line, was moved off it, and leapt to a seat under the footer and back up as the next pass
/// caught up (two leaps in 0.65 s of the harness's first turn). Carried with its rows, the late
/// chrome draws the cat a host in time draws: the same body, pose and alpha on every present of
/// every take, at 120 and 60 Hz, the echo as recorded and 150 ms late.
///
/// NEGATIVE CONTROL (as run before the fix): the generating take parts at 0.77 s, in the
/// session's first bound (its landing aimed off the chrome a frame late); further on it hopped at
/// 11.32 s where the host in time rides the composer down, and at 12.26 s the cat was moved off
/// the stale line (`cursor-home`) and leapt.
#[test]
fn late_chrome_draws_the_cat_chrome_in_time_draws() {
    let drawn = |s: &Shot| {
        let f = s.frame;
        (
            s.ms,
            f.action,
            f.pose,
            f.col,
            f.row,
            f.lift,
            f.alpha,
            f.facing_left,
        )
    };
    for (take, shot_take) in TAKES.iter().enumerate() {
        for (frame_ms, echo_ms) in [(8u64, 0u64), (16, 0), (16, 150)] {
            let in_time = Knobs {
                frame_ms,
                echo_ms,
                chips: true,
                brand: true,
                ..Knobs::SHIPPED
            };
            let late = Knobs {
                chrome_lag: true,
                ..in_time
            };
            let (a, b) = (replayed(take, in_time), replayed(take, late));
            let parted = a
                .iter()
                .zip(b)
                .find(|(x, y)| drawn(x) != drawn(y))
                .map(|(x, y)| (drawn(x), x.reason, drawn(y), y.reason));
            assert!(
                parted.is_none() && a.len() == b.len(),
                "{} at {frame_ms} ms a frame, echo +{echo_ms} ms: the late chrome parts the cat \
                 from the chrome in time at (in time, late) {parted:?}",
                shot_take.name
            );
        }
    }
}

/// THE SEAT UNDER THE FOOTER RIDES THE COMPOSER DOWN. With the harness the cat
/// rests on the clean ground under Claude Code's footer. As the session's first
/// turn ends, its last line pushes the composer down a row, the footer's words
/// changing as it goes (` · esc to interrupt` gone), so no row round the cat moved
/// whole: the console's seat goes down the row with the composer, and the cat
/// rides with it in the frame the composer moves. Hopped after its seat, it
/// bounded a row down. At 120 and 60 Hz, the echo as recorded and 150 ms late: a
/// ride as the turn ends (11.2 s to 11.5 s, shifted by the echo) and no flight.
///
/// NEGATIVE CONTROL (as run before the fix): a hop at 11.33 s from row 15.85 to
/// row 16.85, no ride; with the echo 150 ms late the cat was not by the composer.
#[test]
fn the_seat_under_the_footer_rides_the_composer_down() {
    for (frame_ms, echo_ms) in [(8u64, 0u64), (16, 0), (8, 150), (16, 150)] {
        let knobs = Knobs {
            frame_ms,
            echo_ms,
            chips: true,
            brand: true,
            ..Knobs::SHIPPED
        };
        let shots = replayed(0, knobs);
        let span = (11_200 + echo_ms)..(11_500 + echo_ms);
        let rode = shots
            .iter()
            .filter(|s| span.contains(&s.ms) && s.reason == "content-ride")
            .map(|s| (s.ms, s.frame.row))
            .collect::<Vec<_>>();
        let flew = flights_of(shots)
            .into_iter()
            .filter(|(at, ..)| span.contains(at))
            .map(|(at, from, landed, to)| (at, from, (landed.col.round(), landed.row), to))
            .collect::<Vec<_>>();
        assert!(
            !rode.is_empty() && flew.is_empty(),
            "{frame_ms} ms a frame, echo +{echo_ms} ms: rides (ms, row) {rode:?}, flights \
             (ms, caret, landing, caret) {flew:?}"
        );
    }
}

/// NO BOUND ON THE SPOT. A gather re-reads its target every tick; one whose target
/// came back within a short walk along its own row (`BOUND_SPAN_MIN`) while it
/// gathered stands up and walks there. Launched, it hopped a cell or two: the
/// long insert's cat bounded from column 54.3 to 53.1 as the arrow keys walked
/// the caret back into the line (the head law's seat at the far margin for a
/// moment had latched a big jump), and from 140.0 to 138.1 at the far margin. No
/// flight in any take lands within three cells of where it left on its own row,
/// at 120 and 60 Hz, the echo as recorded and 150 ms late, plain, with the chips
/// and with the harness, and the long insert's dictation typed as keys.
///
/// NEGATIVE CONTROL (as run before the fix): six such hops in the long insert.
#[test]
fn no_flight_is_a_hop_on_the_spot() {
    let hosts = [
        Knobs::SHIPPED,
        Knobs {
            chips: true,
            ..Knobs::SHIPPED
        },
        Knobs {
            chips: true,
            brand: true,
            ..Knobs::SHIPPED
        },
    ];
    let mut runs: Vec<(usize, Knobs, bool)> = Vec::new();
    for take in 0..TAKES.len() {
        for host in hosts {
            for (frame_ms, echo_ms) in [(8u64, 0u64), (16, 0), (8, 150), (16, 150)] {
                let knobs = Knobs {
                    frame_ms,
                    echo_ms,
                    ..host
                };
                runs.push((take, knobs, false));
                if take == 3 && echo_ms == 0 {
                    runs.push((take, knobs, true));
                }
            }
        }
    }
    let seen = crate::common::par_map(&runs, |&(take, knobs, keys)| {
        let typed;
        let shots: &[Shot] = if keys {
            typed = Host::replay_events(&TAKES[take], knobs, long_insert_by_keys()).shots;
            &typed
        } else {
            replayed(take, knobs)
        };
        let hops = shots
            .windows(2)
            .enumerate()
            .filter(|(_, w)| {
                w[1].frame.action == PetAction::Leap && w[0].frame.action != PetAction::Leap
            })
            .filter_map(|(i, w)| {
                let from = (w[0].frame.col, w[0].frame.row);
                let landed = shots[i + 1..]
                    .iter()
                    .find(|s| s.frame.action != PetAction::Leap)?;
                let to = (landed.frame.col, landed.frame.row);
                ((to.1 - from.1).abs() < 0.5 && (to.0 - from.0).abs() < 3.0)
                    .then_some((w[1].ms, from, to))
            })
            .collect::<Vec<_>>();
        (
            format!(
                "{} at {} ms, echo +{} ms, chips {}, harness {}, keys {keys}",
                TAKES[take].name, knobs.frame_ms, knobs.echo_ms, knobs.chips, knobs.brand
            ),
            hops,
        )
    });
    for (label, hops) in seen {
        assert!(
            hops.is_empty(),
            "{label}: flights landing within three cells of where they left (ms, from, to): \
             {hops:?}"
        );
    }
}

/// A late-chrome run ([`late_chrome_runs`]): its label, its flights, and the presents
/// `(ms, console reason)` on which the host moved the body off chrome or hid it there (the
/// console's `cursor-home` and `protected-home-*`).
type LateChromeRun = (String, Vec<Flown>, Vec<(u64, &'static str)>);

/// The session's first 10 s with the harness's chrome laid at each of `froms` ms into the take,
/// at 120 and 60 Hz with the echo late by each of `echoes`.
fn late_chrome_runs(froms: &[u64], echoes: &[u64]) -> Vec<LateChromeRun> {
    let mut runs: Vec<(String, Knobs)> = Vec::new();
    for frame_ms in [8u64, 16] {
        for &echo_ms in echoes {
            for &chrome_from_ms in froms {
                runs.push((
                    format!(
                        "{frame_ms} ms a frame, echo +{echo_ms} ms, chrome at {chrome_from_ms} ms"
                    ),
                    Knobs {
                        frame_ms,
                        echo_ms,
                        chips: true,
                        brand: true,
                        chrome_from_ms,
                        ..Knobs::SHIPPED
                    },
                ));
            }
        }
    }
    crate::common::par_map(&runs, |(label, knobs)| {
        let h = Host::replay(&TAKES[0], *knobs);
        let early: Vec<Shot> = h.shots.iter().filter(|s| s.ms < 10_000).copied().collect();
        let moved = early
            .iter()
            .filter(|s| s.reason == "cursor-home" || s.reason.starts_with("protected-home"))
            .map(|s| (s.ms, s.reason))
            .collect();
        (label.clone(), flights_of(&early), moved)
    })
}

/// CHROME LAID UNDER THE FIRST BOUND TAKES ITS LANDING. aterm lays the harness's lockup and
/// switch over Claude Code's composer rule, and its chips, a beat after the composer's first
/// frame, once it has told the session is Claude Code's: in a window, while the session's
/// first bound is in the air. The bound comes down clear of the chrome, beside the
/// composer's line, in the one flight; the host never has to move it off the chrome.
/// Judged with the chrome laid 500 and 600 ms into the take, at 120 and 60 Hz, with the echo
/// 10 and 150 ms late.
///
/// NEGATIVE CONTROL (as run before the fix): with the echo 10 ms late the bound came down
/// beside the prompt under the lockup, the host moved it to the footer in one present, and it
/// leapt to the line: two flights.
#[test]
fn chrome_laid_under_the_first_bound_takes_its_landing() {
    for (label, flights, moved) in late_chrome_runs(&[500, 600], &[10, 150]) {
        assert!(
            flights.len() <= 1 && moved.is_empty(),
            "{label}: flights (ms, caret, landing, caret) {flights:?}; the body moved off \
             chrome (ms, reason) {moved:?}"
        );
    }
}

/// CHROME LAID OVER THE RESTING CAT IS HOPPED OFF. The harness's chrome laid only once the
/// cat has come down beside the prompt covers its head: it hops off to its seat beside the
/// composer's line, as off ink. The host moving it off the chrome in one present is a jump
/// on the spot. Judged with the chrome laid 1 s and 1.2 s into the take, at 120 and 60 Hz,
/// with the echo as recorded, 10 and 150 ms late.
///
/// NEGATIVE CONTROL (as run before the fix): the host moved the body off the chrome
/// (`cursor-home`) as it landed or sat, and the cat leapt from where it was put.
#[test]
fn chrome_laid_over_the_resting_cat_is_hopped_off() {
    for (label, flights, moved) in late_chrome_runs(&[1_000, 1_200], &[0, 10, 150]) {
        assert!(
            moved.is_empty() && flights.len() <= 2,
            "{label}: the body moved off chrome (ms, reason) {moved:?}; flights {flights:?}"
        );
    }
}

/// A host that pastes `text` into the composer at 3.36 s and submits it
/// `submit_after` ms later; the transcript takes the prompt and the cleared
/// composer goes down under it. The keys are noted as the app notes them: the
/// paste as a paste, the Enter as a submit.
fn pasted_and_submitted(submit_after: u64, frame_ms: u64) -> Host {
    let text = "please read the design notes in docs and tell me what the footer should do \
                when the composer wraps past the right edge of the window";
    let window = Take {
        name: "paste",
        src: "",
        rows: 40,
        cols: 120,
    };
    let composer = |draft: &str, top: usize| -> Vec<u8> {
        const COLS: usize = 120;
        let rule = "\u{2500}".repeat(COLS);
        let footer = "  \u{23f5}\u{23f5} auto mode on (shift+tab to cycle)";
        let chars: Vec<char> = draft.chars().collect();
        let lines: Vec<String> = if chars.is_empty() {
            vec![String::new()]
        } else {
            chars.chunks(COLS - 2).map(|c| c.iter().collect()).collect()
        };
        let mut out = String::from("\x1b[?2026h");
        for row in top..top + 8 {
            out.push_str(&format!("\x1b[{};1H\x1b[2K", row + 1));
        }
        out.push_str(&format!("\x1b[{top};1H{rule}"));
        for (i, line) in lines.iter().enumerate() {
            let lead = if i == 0 { "\u{276f} " } else { "  " };
            out.push_str(&format!("\x1b[{};1H{lead}{line}", top + 1 + i));
        }
        let last = lines.len() - 1;
        let caret_col = 2 + lines[last].chars().count();
        let lower = top + lines.len();
        out.push_str(&format!(
            "\x1b[{};1H{rule}\x1b[{};1H{footer}\x1b[{};{}H\x1b[?2026l",
            lower + 1,
            lower + 2,
            top + 1 + last,
            caret_col + 1
        ));
        out.into_bytes()
    };
    let mut ev: Vec<(u64, Rec, Option<PetInputKind>)> = Vec::new();
    let mut screen = String::from("\x1b[2J\x1b[H\x1b[?25l");
    screen.push_str(
        "\x1b[1;1H \u{2590}\u{259b}\u{2588}\u{2588}\u{2588}\u{259c}\u{258c}   Claude Code v2.1.305",
    );
    screen.push_str("\x1b[2;1H\u{259d}\u{259c}\u{2588}\u{2588}\u{2588}\u{2588}\u{2588}\u{259b}\u{2598}  Opus 5.5 \u{b7} API Usage Billing");
    screen.push_str("\x1b[3;1H  \u{2598}\u{2598} \u{259d}\u{259d}    /private/tmp/lab/proj");
    ev.push((0, Rec::Out(screen.into_bytes()), None));
    ev.push((10, Rec::Out(composer("", 5)), None));
    let mut t = 1_000;
    for (n, ch) in "hi ".char_indices() {
        ev.push((
            t,
            Rec::In(ch.to_string().into_bytes()),
            Some(PetInputKind::Text),
        ));
        ev.push((t + 15, Rec::Out(composer(&"hi "[..=n], 5)), None));
        t += 120;
    }
    t += 2_000;
    let draft = format!("hi {text}");
    ev.push((t, Rec::In(Vec::new()), Some(PetInputKind::Paste)));
    ev.push((t + 20, Rec::Out(composer(&draft, 5)), None));
    t += submit_after;
    ev.push((t, Rec::In(b"\r".to_vec()), Some(PetInputKind::Submit)));
    let mut sub = String::from("\x1b[?2026h");
    sub.push_str(&format!("\x1b[6;1H\x1b[2K> hi {}", &text[..100]));
    sub.push_str(&format!("\x1b[7;1H\x1b[2K  {}", &text[100..]));
    sub.push_str("\x1b[9;1H\x1b[2K\u{25cf} Reading the notes\u{2026}");
    sub.push_str("\x1b[?2026l");
    ev.push((t + 30, Rec::Out(sub.into_bytes()), None));
    ev.push((t + 60, Rec::Out(composer("", 11)), None));
    ev.push((t + 3_000, Rec::Out(Vec::new()), None));
    let mut h = Host::new(
        &window,
        Knobs {
            frame_ms,
            ..Knobs::SHIPPED
        },
    );
    for (ms, rec, kind) in ev {
        h.run_to(ms);
        if let Some(k) = kind
            && k != PetInputKind::Text
        {
            h.owner.note_console_input(h.now, k);
        }
        match rec {
            Rec::In(b) => {
                if kind == Some(PetInputKind::Text) {
                    h.key(&b);
                }
            }
            Rec::Out(b) => h.feed(&b),
        }
    }
    let end = h.ms() + 1_000;
    h.run_to(end);
    h
}

/// A PASTE SUBMITTED AT ONCE IS ONE TRIP. A program that pastes a prompt into
/// Claude Code's composer and presses Enter within half a second (the harness,
/// an agent's driver) moves the caret three times: to the end of the pasted
/// text, then, with the submit's repaint, onto the transcript and down to the
/// cleared composer under it. The cat leapt at the paste's jump, came down on a
/// row the submit had left and leapt again. The paste hands the caret to the
/// program until it settles (`PASTE_SETTLE`): one flight, onto the caret's
/// row, with the Enter 120 to 450 ms after the paste, at 120 and 60 Hz.
///
/// NEGATIVE CONTROL (as run on main before the fix): 2 flights in every case,
/// the first landing on row 6 or 8 with the caret on row 11 below 450 ms.
#[test]
fn a_paste_submitted_at_once_is_one_trip() {
    let runs: Vec<(u64, u64)> = [120u64, 200, 300, 450]
        .into_iter()
        .flat_map(|after| [(after, 8u64), (after, 16)])
        .collect();
    let seen = crate::common::par_map(&runs, |&(after, frame_ms)| {
        let h = pasted_and_submitted(after, frame_ms);
        let flights: Vec<_> = flights_of(&h.shots)
            .into_iter()
            .filter(|(at, ..)| *at >= 3_300)
            .map(|(at, from, landed, to)| (at, from, (landed.col.round(), landed.row), to))
            .collect();
        (after, frame_ms, flights)
    });
    for (after, frame_ms, flights) in seen {
        let label = format!("Enter {after} ms after the paste, {frame_ms} ms a frame");
        assert_eq!(
            flights.len(),
            1,
            "{label}: flights (ms, caret, landing at (col, row), caret): {flights:?}"
        );
        let (_, _, (_, row), caret) = flights[0];
        assert!(
            (row - f32::from(caret.0)).abs() < 0.5,
            "{label}: came down on row {row} with the caret on row {}",
            caret.0
        );
    }
}

/// A PASTE SUBMITTED LATER COMES DOWN ON THE ROW THE PROMPT WENT TO. Enter
/// pressed 800 ms after the paste comes while the cat is coming down to the
/// pasted text's end: the cleared composer's prompt lies behind it, under the
/// transcript. The bound never turns back in the air; it takes the prompt's row
/// and keeps its column, and goes along the row to the caret from there. Kept on
/// the row the submit's repaint had left, it came down there and leapt again.
/// At 120 and 60 Hz: every landing on the caret's row.
///
/// NEGATIVE CONTROL (as run before the fix): the first landing on row 8, the
/// caret on row 11.
#[test]
fn a_paste_submitted_later_comes_down_on_the_row_the_prompt_went_to() {
    for frame_ms in [8u64, 16] {
        let h = pasted_and_submitted(800, frame_ms);
        for (at, from, landed, to) in flights_of(&h.shots)
            .into_iter()
            .filter(|(at, ..)| *at >= 3_300)
        {
            assert!(
                (landed.row - f32::from(to.0)).abs() < 0.5,
                "{frame_ms} ms a frame: the flight at {at} ms (caret {from:?}) came down on row \
                 {} with the caret on row {}",
                landed.row,
                to.0
            );
        }
    }
}

/// The first key the take's recording sends, in ms.
fn first_key_ms(take: &Take) -> u64 {
    recording(take.src, 1_000_000)
        .into_iter()
        .find_map(|(ms, ev)| match ev {
            Rec::In(b) if b.len() == 1 && (0x20..0x7f).contains(&b[0]) => Some(ms),
            _ => None,
        })
        .expect("the take types")
}

/// THE FIRST KEY COSTS NO FLIGHT UNDER THE HARNESS. The harness's top rule,
/// which the host holds whole, keeps every stand from under it: the escort's
/// stand goes on top of it (aterm-effects SPECS `host-line-head-law`). The cat
/// rested above the composer and flew down to the line each time typing began;
/// it waits now where typing finds it, and a first key that comes while the
/// cat is still in the air takes its landing to the escort's stand: no flight
/// leaves from the first key until the writing has gone 1.5 s, in the composer
/// and wrap-end takes and in the bracketed composer, at 120 and 60 Hz, echo as
/// recorded and 150 ms late.
///
/// NEGATIVE CONTROL (as run on main before the fix): one flight 0 to 10 ms
/// after the first key in every host, from row 34.85 or 8.85 down to the
/// caret's row.
#[test]
fn the_first_key_costs_no_flight_under_the_harness() {
    let harness = Knobs {
        chips: true,
        brand: true,
        ..Knobs::SHIPPED
    };
    let runs: Vec<(usize, u64, u64)> = [1usize, 2]
        .into_iter()
        .flat_map(|take| {
            [(8u64, 0u64), (16, 0), (8, 150), (16, 150)]
                .into_iter()
                .map(move |(f, e)| (take, f, e))
        })
        .collect();
    let mut seen = crate::common::par_map(&runs, |&(take, frame_ms, echo_ms)| {
        let knobs = Knobs {
            frame_ms,
            echo_ms,
            ..harness
        };
        let key = first_key_ms(&TAKES[take]);
        let flights: Vec<_> = flights_of(replayed(take, knobs))
            .into_iter()
            .filter(|(at, ..)| (key..=key + 1_500).contains(at))
            .map(|(at, ..)| at)
            .collect();
        (
            format!("{} at {frame_ms} ms, echo +{echo_ms} ms", TAKES[take].name),
            key,
            flights,
        )
    });
    let h = Host::replay_events(
        &Take {
            name: "bracketed",
            src: "",
            rows: 40,
            cols: 120,
        },
        harness,
        bracketed_composer_take(),
    );
    seen.push((
        "bracketed at 8 ms".to_string(),
        1_000,
        flights_of(&h.shots)
            .into_iter()
            .filter(|(at, ..)| (1_000..=2_500).contains(at))
            .map(|(at, ..)| at)
            .collect(),
    ));
    for (label, key, flights) in seen {
        assert!(
            flights.is_empty(),
            "{label}: flights leaving after the first key at {key} ms: {flights:?}"
        );
    }
}

/// A JUMP A PROGRAM TAKES STRAIGHT BACK IS NO TRIP. Between turns a program
/// types a prompt into Claude Code's composer and submits it at once (`go
/// slowly`, 340 ms later): the caret runs nine cells along the row and comes
/// back. The cat pounced at the first jump and turned round in the air for the
/// second. The coil waits out a lone jump along the row (`LONE_SETTLE`) and,
/// the caret back where the cat stands, leaves nowhere: no flight from 39.3 s
/// to 40.6 s, at 120 and 60 Hz, echo as recorded and 150 ms late, plain and
/// with the chips. With the harness the cat rests on clean ground on top of
/// the composer's upper rule, past the transcript's last line, and the
/// program's prompt calls it to the caret's column: one flight at most, to
/// where the caret came back to.
///
/// NEGATIVE CONTROL (as run on main before the fix): one flight in every host,
/// out toward column 12 and turned back mid-air to column 3.
#[test]
fn a_jump_a_program_takes_straight_back_is_no_trip() {
    for (label, echo, harness, flights, _) in generating_flights() {
        let out_and_back: Vec<_> = flights
            .iter()
            .filter(|(at, ..)| (39_300 + echo..=40_600 + echo).contains(at))
            .map(|(at, from, landed, to)| (*at, *from, (landed.col.round(), landed.row), *to))
            .collect();
        assert!(
            out_and_back.len() <= usize::from(harness)
                && out_and_back.iter().all(|(_, _, _, to)| to.1 < 9),
            "{label}: flights (ms, caret, landing at (col, row), caret): {out_and_back:?}"
        );
    }
}

/// How long each flight of `shots` is in the air: `(the present it leaves on, ms aloft)`.
fn airtimes(shots: &[Shot]) -> Vec<(u64, u64)> {
    shots
        .windows(2)
        .enumerate()
        .filter(|(_, w)| {
            w[1].frame.action == PetAction::Leap && w[0].frame.action != PetAction::Leap
        })
        .filter_map(|(i, w)| {
            let down = shots[i + 1..]
                .iter()
                .find(|s| s.frame.action != PetAction::Leap)?;
            Some((w[1].ms, down.ms - w[1].ms))
        })
        .collect()
}

/// The big bound's longest shape (ms): no crossing the writing's wrap makes is drawn out
/// past it.
const WRAP_BOUND_MS_MAX: u64 = 850;

/// THE WRITING'S WRAP IS A BRISK BOUND. In the generating take Claude Code's composer
/// word-wraps the writing at 19.9, 30.7 and 61.1 s (a program types the first two lines into
/// it, the hand the third): the caret goes from the margin back to the next line's start,
/// 110 cells, and the writing runs on at 20 keys a second. The cat it left at the margin
/// expected the wrap: it goes at once, no notice beat (it leaves within 200 ms), and crosses
/// in the bound the animal's own pace makes, in the air no longer than the big bound's longest
/// shape, at 120 and 60 Hz, the echo as recorded and 150 ms late, plain, with the chips, with
/// the harness, and with the harness's chrome a present late. So does the long insert's wrap at
/// 26.9 s, where the word the wrap broke comes down with the caret a present or more after the
/// caret went to the line's start (8 cells on), each take also under a window's present timing
/// (a present a vsync, two or three after the one before, late by up to 3 ms, 3 schedules).
///
/// NEGATIVE CONTROL (as run on main before the fix): every wrap's bound 1.06 to 1.17 s in the
/// air, paced as a quiet crossing (0.74 of the pane a second); with the harness the cat, sat at
/// the margin, perked first and left 424 ms after the wrap. With the word's step read as a jump
/// that ended the wrap, the long insert's cat at the far margin crossed 101 cells in 0.96 s at
/// 60 Hz whenever a present showed the step before the cat sprang.
#[test]
fn the_writings_wrap_is_a_brisk_bound() {
    let hosts = [
        Knobs::SHIPPED,
        Knobs {
            chips: true,
            ..Knobs::SHIPPED
        },
        Knobs {
            chips: true,
            brand: true,
            ..Knobs::SHIPPED
        },
        Knobs {
            chips: true,
            brand: true,
            chrome_lag: true,
            ..Knobs::SHIPPED
        },
    ];
    // `(take, the wraps it holds)`: the generating take, and the long insert.
    let mut runs: Vec<(usize, usize, Knobs)> = Vec::new();
    for (take, wraps) in [(0usize, 3usize), (3, 1)] {
        for host in hosts {
            for (frame_ms, echo_ms) in [(8u64, 0u64), (16, 0), (8, 150), (16, 150)] {
                for present_seed in 0..=3u64 {
                    runs.push((
                        take,
                        wraps,
                        Knobs {
                            frame_ms,
                            echo_ms,
                            present_seed,
                            ..host
                        },
                    ));
                }
            }
        }
    }
    let seen = crate::common::par_map(&runs, |&(take, wraps_held, knobs)| {
        let shots = replayed(take, knobs);
        let cols = TAKES[take].cols;
        // A wrap: the caret from within six cells of the margin to the first twelve of the
        // same row (a composer at the screen's foot grows up) or the next.
        let wraps: Vec<u64> = shots
            .windows(2)
            .filter(|w| {
                let ((pr, pc), (r, c)) = (w[0].caret, w[1].caret);
                (r == pr || r == pr + 1) && pc + 6 >= cols && c <= 12
            })
            .map(|w| w[1].ms)
            .collect();
        let flights = airtimes(shots);
        let crossings: Vec<(u64, Option<(u64, u64)>)> = wraps
            .iter()
            .map(|&w| {
                (
                    w,
                    flights
                        .iter()
                        .copied()
                        .find(|&(at, _)| at >= w && at <= w + 1_500),
                )
            })
            .collect();
        (
            format!(
                "{}: {} ms a frame, echo +{} ms, chips {}, harness {}, chrome late {}, present \
                 schedule {}",
                TAKES[take].name,
                knobs.frame_ms,
                knobs.echo_ms,
                knobs.chips,
                knobs.brand,
                knobs.chrome_lag,
                knobs.present_seed
            ),
            wraps_held,
            crossings,
        )
    });
    for (label, wraps_held, crossings) in seen {
        assert!(
            crossings.len() >= wraps_held,
            "{label}: the take's {wraps_held} wraps, seen {crossings:?}"
        );
        for &(wrap, flight) in &crossings {
            let Some((at, aloft)) = flight else {
                panic!("{label}: no bound for the wrap at {wrap} ms");
            };
            assert!(
                at - wrap <= 200 && aloft <= WRAP_BOUND_MS_MAX,
                "{label}: the wrap at {wrap} ms: the bound left {} ms after it and was {aloft} ms \
                 in the air",
                at - wrap
            );
        }
    }
}

/// The long insert's events as recorded, to the end of its take.
fn long_insert_as_recorded() -> Vec<(u64, Rec)> {
    let mut out = Vec::new();
    for (ms, ev) in recording(TAKES[3].src, 1_000_000) {
        let end = matches!(&ev, Rec::In(b) if b.as_slice() == b"\x1b[99;5u");
        out.push((ms, ev));
        if end {
            break;
        }
    }
    out
}

/// A CARET AT REST SENDS A FAR CAT ONCE TO ITS SEAT. At 6.74 s the long insert's dictation
/// stops with the caret at the end of the composer's text, every line above it lettered to
/// the margin; the cat is off by the dictation's letters, or watching from the far margin.
/// No stand by the caret keeps its head off the letters; the seat that does is on the footer
/// under the caret, two rows down (the far margin is 85 cells off). The cat goes there once,
/// from wherever it is, before the arrow keys begin at 8.4 s: one flight, never one that comes
/// down where it rose, landing on the footer's row, under a window's present timing (a present
/// a vsync, two or three after the one before, late by up to 3 ms, 16 schedules), at 120 and
/// 60 Hz, plain, with the chips and with the harness, their chrome a present late.
///
/// NEGATIVE CONTROL (as run on main before the fix): 82 of the 96 runs failed. 17 took two
/// flights, the second a rise that came down where the cat stood (the window's hop at column
/// 138: the stand by the caret read before the caret had rested half a second, the seat at the
/// far margin after); the rest came down by the caret on its line, the head among the letters,
/// or off the letters at the far margin.
#[test]
fn a_caret_at_rest_sends_a_far_cat_once_to_its_seat() {
    let hosts = [
        Knobs::SHIPPED,
        Knobs {
            chips: true,
            chrome_lag: true,
            ..Knobs::SHIPPED
        },
        Knobs {
            chips: true,
            brand: true,
            chrome_lag: true,
            ..Knobs::SHIPPED
        },
    ];
    let mut runs: Vec<Knobs> = Vec::new();
    for host in hosts {
        for frame_ms in [8u64, 16] {
            for present_seed in 1..=16u64 {
                runs.push(Knobs {
                    frame_ms,
                    present_seed,
                    ..host
                });
            }
        }
    }
    let seen = crate::common::par_map(&runs, |&knobs| {
        let h = Host::replay_events(&TAKES[3], knobs, long_insert_as_recorded());
        let mut out = Vec::new();
        for (i, w) in h.shots.windows(2).enumerate() {
            if !(w[1].frame.action == PetAction::Leap && w[0].frame.action != PetAction::Leap)
                || !(6_740..8_390).contains(&w[1].ms)
            {
                continue;
            }
            let rose = (w[0].frame.col, w[0].frame.row);
            if let Some(down) = h.shots[i + 1..]
                .iter()
                .find(|s| s.frame.action != PetAction::Leap)
            {
                out.push((w[1].ms, rose, (down.frame.col, down.frame.row)));
            }
        }
        (
            format!(
                "{} ms a frame, present schedule {}, chips {}, harness {}",
                knobs.frame_ms, knobs.present_seed, knobs.chips, knobs.brand
            ),
            out,
        )
    });
    for (label, flights) in seen {
        let on_the_spot = flights
            .iter()
            .any(|&(_, (c0, r0), (c1, r1))| (r1 - r0).abs() < 0.5 && (c1 - c0).abs() < 3.0);
        assert!(
            flights.len() <= 1
                && !on_the_spot
                && flights
                    .iter()
                    .all(|&(_, _, (_, row))| (row - 59.0).abs() < 0.5),
            "{label}: flights from the dictation's end to the arrow keys (ms, from, to): \
             {flights:?}"
        );
    }
}

/// A busy Claude Code on the main screen ([`busy_claude`]).
#[derive(Clone, Copy, Debug)]
struct BusyClaude {
    rows: usize,
    cols: usize,
    /// Blank rows between the spinner and the composer's upper rule.
    gap: usize,
    /// The transcript fills the screen over the live region from the first frame.
    full: bool,
    /// The lengths of the two footer rows under the bottom rule.
    feet: (usize, usize),
    /// The account's name at the bottom rule's right end, this many letters of it.
    account: usize,
    /// The spinner line's length.
    spin: usize,
    /// Each frame hides the cursor while it redraws and shows it at the caret again.
    hide: bool,
    /// Every spinner frame redraws the whole live region, not only the spinner's row.
    whole: bool,
    /// The mean time between transcript writes (ms), and the seed of their lengths.
    lines: (u64, u64),
    /// The mean pause between bursts of letters typed into the composer while Claude Code
    /// works (ms), seeded from [`Self::lines`]'s seed; 0: nobody types.
    keys: u64,
}

/// [`BusyClaude`]'s take, `secs` long: Claude Code's live region (the spinner, the composer's
/// rules round its input line, the caret parked on that line's third column, two footer rows)
/// at the bottom of a main screen whose transcript grows over it, a line or three at a time,
/// and scrolls the screen once it reaches the bottom; the spinner redraws every 50 ms in its
/// synchronized frames between them. With [`BusyClaude::keys`], someone types into the
/// composer meanwhile, bursts of letters a fifth of a second or less apart, now and then
/// cleared, each echoed on the input line 6 ms after its key.
fn busy_claude(b: BusyClaude, secs: u64) -> Vec<(u64, Rec)> {
    const WORDS: &str = "the turn reads the files and writes its answer over the composer while the spinner counts on ";
    const FOOT: [&str; 2] = [
        "  5h 0% · 7d 3% · bypass permissions on (shift+tab to cycle) · esc to interrupt · more words here ok",
        "  YOLO on · 1 shell · and some more footer words to make it long enough for a test",
    ];
    const ACCOUNT: &str = "andrew@example.com";
    let cols = b.cols;
    let rule = "─".repeat(cols);
    let take = |text: &str, n: usize| -> String { text.chars().take(n.min(cols - 1)).collect() };
    let feet = [take(FOOT[0], b.feet.0), take(FOOT[1], b.feet.1)];
    let xorshift = |seed: u64| {
        let mut x = seed | 1;
        move |m: u64| {
            x ^= x << 13;
            x ^= x >> 7;
            x ^= x << 17;
            x % m
        }
    };
    let mut rnd = xorshift(b.lines.1.wrapping_mul(0x9E37_79B9_7F4A_7C15));
    let mut key_rnd = xorshift(b.lines.1.wrapping_mul(0x2545_F491_4F6C_DD1D));
    let live = |spin: &str, typed: &str| -> String {
        let mut rows = vec![spin.to_string()];
        rows.extend(std::iter::repeat_n(String::new(), b.gap));
        let account = take(ACCOUNT, b.account);
        let lower = if account.is_empty() {
            rule.clone()
        } else {
            let n = account.chars().count();
            rule.chars().take(cols - n - 4).collect::<String>() + " " + &account + " ──"
        };
        rows.extend([rule.clone(), format!("❯\u{a0}{typed}"), lower]);
        rows.extend(feet.iter().cloned());
        rows.join("\r\n")
    };
    let height = 6 + b.gap;
    let input = 2 + b.gap;
    let open = if b.hide {
        "\x1b[?2026h\x1b[?25l"
    } else {
        "\x1b[?2026h"
    };
    let close = |top: usize, typed: &str| {
        let show = if b.hide { "\x1b[?25h" } else { "" };
        let col = 3 + typed.len();
        format!("\x1b[{};{col}H{show}\x1b[?2026l", top + input + 1)
    };
    let mut screen = String::from("\x1b[2J\x1b[H");
    let mut top = 0;
    for r in 0..if b.full { b.rows - height - 1 } else { 3 } {
        let line: String = WORDS
            .chars()
            .cycle()
            .skip(r * 7)
            .take((30 + r * 37 % 85).min(cols - 3))
            .collect();
        screen.push_str(&format!("  {line}\r\n"));
        top += 1;
    }
    screen.push_str(&format!(
        "{open}\x1b[{};1H\x1b[J{}{}",
        top + 1,
        live("✻ Grooving…", ""),
        close(top, "")
    ));
    let mut ev = vec![(0, Rec::Out(screen.into_bytes()))];
    let glyphs = ['·', '✢', '✳', '✶', '✻', '✽'];
    let (mut t, mut write_at) = (300, 300 + b.lines.0);
    let mut typed = String::new();
    let mut key_at = if b.keys == 0 {
        u64::MAX
    } else {
        300 + key_rnd(b.keys)
    };
    for i in 0usize.. {
        if t > secs * 1_000 {
            break;
        }
        while key_at <= t {
            if typed.len() + 8 >= cols || key_rnd(9) == 0 {
                ev.push((key_at, Rec::In(b"\x15".to_vec())));
                typed.clear();
            } else {
                ev.push((key_at, Rec::In(b"a".to_vec())));
                typed.push('a');
            }
            let echo = format!(
                "{open}\x1b[{};1H\x1b[2K❯\u{a0}{typed}{}",
                top + input + 1,
                close(top, &typed)
            );
            ev.push((key_at + 6, Rec::Out(echo.into_bytes())));
            key_at += if key_rnd(5) == 0 {
                b.keys / 2 + key_rnd(b.keys * 2)
            } else {
                60 + key_rnd(140)
            };
        }
        let spin = format!(
            "{} Grooving… ({}s · ↓ {} tokens · esc to interrupt) and thinking about what to write next",
            glyphs[i % glyphs.len()],
            t / 1_000,
            100 + 7 * i
        );
        let spin = take(&spin, b.spin);
        let frame = if t >= write_at {
            write_at = t + b.lines.0 / 2 + rnd(b.lines.0.max(1));
            let mut out = format!("{open}\x1b[{};1H\x1b[J", top + 1);
            let n = 1 + rnd(3) as usize;
            for _ in 0..n {
                let line: String = WORDS
                    .chars()
                    .cycle()
                    .skip(rnd(50) as usize)
                    .take((10 + rnd(100) as usize).min(cols - 3))
                    .collect();
                out.push_str(&format!("  {line}\r\n"));
            }
            top = (top + n).min(b.rows - height);
            out + &live(&spin, &typed) + &close(top, &typed)
        } else if b.whole {
            format!(
                "{open}\x1b[{};1H\x1b[J{}{}",
                top + 1,
                live(&spin, &typed),
                close(top, &typed)
            )
        } else {
            format!(
                "{open}\x1b[{};1H\x1b[2K{spin}{}",
                top + 1,
                close(top, &typed)
            )
        };
        ev.push((t, Rec::Out(frame.into_bytes())));
        t += 50;
    }
    ev
}

/// The longest stretch of consecutive presents that drew the legs (a walk or a run frame) over a
/// body that stayed within half a cell, held on glass until the next present, among those that
/// ran in place ([`IN_PLACE_MS`], [`IN_PLACE_TURNS`]): `(from ms, ms, changes of the legs' art,
/// col, row, glyph)`. `None` where no stretch ran in place.
fn legs_in_place(shots: &[Shot], end: u64) -> Option<(u64, u64, usize, f32, f32, &'static str)> {
    let legs = |s: &Shot| s.frame.alpha > 0 && gait(&s.frame).is_some();
    let mut worst: Option<(u64, u64, usize, f32, f32, &'static str)> = None;
    for (i, first) in shots.iter().enumerate().filter(|(_, s)| legs(s)) {
        let (mut lo, mut hi) = (
            (first.frame.col, first.frame.row),
            (first.frame.col, first.frame.row),
        );
        let (mut pose, mut turns) = (first.frame.pose, 0);
        let mut j = i + 1;
        while let Some(s) = shots.get(j).filter(|s| legs(s)) {
            let (l, h) = (
                (lo.0.min(s.frame.col), lo.1.min(s.frame.row)),
                (hi.0.max(s.frame.col), hi.1.max(s.frame.row)),
            );
            if h.0 - l.0 >= 0.5 || h.1 - l.1 >= 0.5 {
                break;
            }
            turns += usize::from(s.frame.pose != pose);
            pose = s.frame.pose;
            (lo, hi) = (l, h);
            j += 1;
        }
        let held = shots.get(j).map_or(end, |s| s.ms) - first.ms;
        if (held >= IN_PLACE_MS || turns >= IN_PLACE_TURNS) && worst.is_none_or(|w| held > w.1) {
            let glyph = aterm_effects::pet_glyphs_gen::PET_GLYPHS[first.frame.pose as usize].id;
            worst = Some((
                first.ms,
                held,
                turns,
                first.frame.col,
                first.frame.row,
                glyph,
            ));
        }
    }
    worst
}

/// THE LEGS CARRY THE BODY: a second of walk or run frames, or two strides of them (eight
/// changes of the legs' four-frame art, [`IN_PLACE_TURNS`]), move the body half a cell. Walking
/// without running in place comes near neither: a slow walk turned about by a key on the caret
/// stays within half a cell up to 0.55 s and four changes of its art (4000 dealt busy Claude
/// Code screens, typed into or not); the cat that ran in place beside a busy Claude Code did
/// so 7.48 s, its art changing 133 times.
const IN_PLACE_MS: u64 = 1_000;

/// The two strides of [`IN_PLACE_MS`]'s law: eight changes of the legs' art.
const IN_PLACE_TURNS: usize = 8;

/// The busy Claude Code the cat rests beside the footer of, under the harness host
/// ([`the_cat_never_runs_in_place_beside_a_busy_claude`]'s first take).
const BUSY_FOOTER_REST: BusyClaude = BusyClaude {
    rows: 43,
    cols: 102,
    gap: 1,
    full: true,
    feet: (33, 62),
    account: 7,
    spin: 84,
    hide: true,
    whole: false,
    lines: (400, 196),
    keys: 0,
};

/// THE CAT NEVER RUNS IN PLACE BESIDE A BUSY CLAUDE CODE (the owner, 2026-10-26: "the cursor
/// kitty pet got stuck running in place"). Claude Code works on its turn: the spinner redraws
/// over the composer, the caret waits on the input line, the transcript grows and scrolls,
/// and the cat rests beside the footer under the composer, where aterm rewrites Claude Code's
/// mode row, its words the host's ([`Knobs::footer_chrome`]; reserved then over the whole
/// extent the rewrite wrote, [`Knobs::footer_whole`], a host this test still runs). Bored there,
/// the cat went to hide behind the end of the row's words, which are the host's: every step the
/// walk took onto them the console's frame pass sent back to the perch beside them, and the
/// hide never arrived, so the cat ran on the spot, its legs turning over and its body never a
/// cell from where it stood, for as long as nobody typed (7.5 s of the 12 s it was bored for in
/// the first take below). NEGATIVE CONTROL (as run before the hide asked the console for the
/// ground behind the words, and before a play the console turns back was given up, the footer
/// reserved whole): the first take ran in place 7480 ms, its legs' art changing 133 times.
#[test]
fn the_cat_never_runs_in_place_beside_a_busy_claude() {
    let first = BUSY_FOOTER_REST;
    let takes = [
        first,
        BusyClaude {
            cols: 120,
            rows: 40,
            feet: (15, 66),
            ..first
        },
        BusyClaude {
            whole: true,
            hide: false,
            lines: (900, 7),
            ..first
        },
        BusyClaude {
            gap: 0,
            spin: 40,
            lines: (150, 33),
            ..first
        },
    ];
    let runs: Vec<(usize, u64, (bool, bool))> = (0..takes.len())
        .flat_map(|take| {
            [
                (8, (true, false)),
                (16, (true, false)),
                (8, (true, true)),
                (16, (true, true)),
                (8, (false, false)),
            ]
            .map(|(ms, footer)| (take, ms, footer))
        })
        .collect();
    let seen = crate::common::par_map(&runs, |&(take, frame_ms, footer)| {
        let (footer_chrome, footer_whole) = footer;
        let b = takes[take];
        let window = Take {
            name: "busy",
            src: "",
            rows: b.rows as u16,
            cols: b.cols as u16,
        };
        let knobs = Knobs {
            chips: true,
            brand: true,
            frame_ms,
            footer_chrome,
            footer_whole,
            ..Knobs::SHIPPED
        };
        let h = Host::replay_events(&window, knobs, busy_claude(b, 15));
        let footer_rest = h
            .shots
            .iter()
            .filter(|s| {
                s.frame.alpha > 0
                    && gait(&s.frame).is_none()
                    && s.rule.is_some_and(|r| s.frame.row.round() as usize > r)
            })
            .count() as u64
            * frame_ms;
        (
            take,
            frame_ms,
            footer,
            footer_rest,
            legs_in_place(&h.shots, h.ms()),
        )
    });
    for whole in [false, true] {
        assert!(
            seen.iter().any(|&(take, _, footer, rest, _)| take == 0
                && footer == (true, whole)
                && rest >= 5_000),
            "fixture: the cat rests beside the host's footer under the composer (reserved \
             whole: {whole}): {seen:?}"
        );
    }
    for (take, frame_ms, footer, _, worst) in &seen {
        assert!(
            worst.is_none(),
            "busy take {take} at {frame_ms} ms a frame, footer chrome (on, whole) {footer:?}: \
             the legs ran in place (from ms, ms, changes of their art, col, row, glyph) \
             {worst:?}"
        );
    }
}

/// THE LAW OF THE LEGS: whatever the cat is doing, a second of walk or run frames on glass, or
/// two strides of their art, carry its body half a cell ([`IN_PLACE_MS`], [`IN_PLACE_TURNS`]);
/// a cat going nowhere is drawn at rest. Over every recorded take, as the shipped host and as
/// the harness host draws them, and over busy Claude Code screens of every shape the deal below
/// makes, typed into or not. NEGATIVE CONTROL (as run before the hide asked the console for the
/// ground behind the words, and before a play the console turns back was given up, the footer
/// reserved whole, [`Knobs::footer_whole`]): the busy footer's take ran in place 4480 ms.
#[test]
fn the_legs_carry_the_body() {
    let mut runs: Vec<(usize, Knobs)> = Vec::new();
    for take in 0..TAKES.len() {
        for knobs in [
            Knobs::SHIPPED,
            Knobs {
                chips: true,
                brand: true,
                ..Knobs::SHIPPED
            },
            Knobs {
                chips: true,
                brand: true,
                footer_chrome: true,
                frame_ms: 16,
                present_seed: 77,
                ..Knobs::SHIPPED
            },
        ] {
            runs.push((take, knobs));
        }
    }
    let footer_rest = Knobs {
        chips: true,
        brand: true,
        footer_chrome: true,
        ..Knobs::SHIPPED
    };
    let takes: Vec<(BusyClaude, Knobs)> = (1..=64u64)
        .map(|seed| {
            let mut x = seed.wrapping_mul(0x9E37_79B9_7F4A_7C15) | 1;
            let mut r = |m: u64| {
                x ^= x << 13;
                x ^= x >> 7;
                x ^= x << 17;
                (x % m) as usize
            };
            let b = BusyClaude {
                rows: 24 + r(26),
                cols: 50 + r(80),
                gap: r(3),
                full: r(2) == 1,
                feet: (6 + r(90), 6 + r(70)),
                account: r(18),
                spin: 12 + r(90),
                hide: r(2) == 1,
                whole: r(2) == 1,
                lines: ([150, 400, 900, 2_500][r(4)], seed),
                keys: [0, 0, 800, 3_000][r(4)],
            };
            let brand = r(2) == 1;
            let knobs = Knobs {
                chips: brand || r(2) == 1,
                brand,
                footer_chrome: r(4) != 0,
                frame_ms: [8, 16][r(2)],
                present_seed: [0, 31][r(2)] as u64,
                footer_words: [MODE_ROW_WORDS, 0][r(2)],
                ..Knobs::SHIPPED
            };
            (b, knobs)
        })
        .chain([
            (BUSY_FOOTER_REST, footer_rest),
            (
                BUSY_FOOTER_REST,
                Knobs {
                    footer_whole: true,
                    ..footer_rest
                },
            ),
        ])
        .collect();
    let recorded = crate::common::par_map(&runs, |&(take, knobs)| {
        let shots = replayed(take, knobs);
        let end = shots.last().map_or(0, |s| s.ms + knobs.frame_ms);
        (
            TAKES[take].name.to_string(),
            knobs,
            legs_in_place(shots, end),
        )
    });
    let busy = crate::common::par_map(&takes, |&(b, knobs)| {
        let window = Take {
            name: "busy",
            src: "",
            rows: b.rows as u16,
            cols: b.cols as u16,
        };
        let h = Host::replay_events(&window, knobs, busy_claude(b, 12));
        (format!("{b:?}"), knobs, legs_in_place(&h.shots, h.ms()))
    });
    for (label, knobs, worst) in recorded.iter().chain(&busy) {
        assert!(
            worst.is_none(),
            "{label} under {knobs:?}: the legs ran in place (from ms, ms, changes of their art, \
             col, row, glyph) {worst:?}"
        );
    }
}

/// A shell prompt on row `row`: `letters` letters typed `gap_ms` apart from 2 s in, and after a
/// 3 s pause, as many backspaces at the same pace, the caret walked back to the prompt; then
/// 6 s of quiet.
fn typed_and_taken_back(row: u16, letters: usize, gap_ms: u64) -> Vec<(u64, Rec)> {
    let mut ev = vec![(
        0,
        Rec::Out(format!("\x1b[2J\x1b[H\x1b[{};1H$ ", row + 1).into_bytes()),
    )];
    let mut t = 2_000;
    for _ in 0..letters {
        ev.push((t, Rec::In(b"a".to_vec())));
        ev.push((t + 5, Rec::Out(b"a".to_vec())));
        t += gap_ms;
    }
    t += 3_000;
    for _ in 0..letters {
        ev.push((t, Rec::In(vec![0x7f])));
        ev.push((t + 5, Rec::Out(b"\x08 \x08".to_vec())));
        t += gap_ms;
    }
    ev.push((t + 6_000, Rec::Out(Vec::new())));
    ev
}

/// THE CAT HOPS THE HOST'S CHIP TO ITS STATION ([`Knobs::row_chip`]). The cat that followed the
/// writing out past a chip of the host's on the caret's row sits there while the line is
/// taken back to the prompt, then goes back to the caret: the frame pass puts its first step
/// onto the chip back on the perch past it, and the cat hops (or pounces) over the chip to its
/// station, never running against it. NEGATIVE CONTROL (as run with the walk the console turns
/// back left to the station, `PetBrain::chrome_hop` never set): every step was put back, and
/// the cat ran on the spot against the chip until the take ended (5648 ms, its legs' art
/// changing 172 times, a 3-cell chip at column 20 under a 60 ms hand).
#[test]
fn the_cat_hops_the_hosts_chip_to_its_station() {
    let runs: Vec<((u16, u16), u64, u64)> = [(20, 3), (40, 8), (30, 15), (36, 24)]
        .into_iter()
        .flat_map(|chip| {
            [(60, 8), (150, 16), (400, 8)].map(|(gap, frame_ms)| (chip, gap, frame_ms))
        })
        .collect();
    let seen = crate::common::par_map(&runs, |&((col, cols), gap, frame_ms)| {
        let window = Take {
            name: "chip",
            src: "",
            rows: 40,
            cols: 120,
        };
        let knobs = Knobs {
            row_chip: (20, col, cols),
            frame_ms,
            ..Knobs::SHIPPED
        };
        let h = Host::replay_events(&window, knobs, typed_and_taken_back(20, 70, gap));
        let past = h.shots.iter().any(|s| s.frame.col >= f32::from(col + cols));
        let home = h.shots.last().map(|s| (s.frame.col, s.frame.row));
        (
            (col, cols, gap, frame_ms),
            past,
            home,
            legs_in_place(&h.shots, h.ms()),
        )
    });
    for (label, past, home, worst) in &seen {
        assert!(
            worst.is_none(),
            "(chip at, cols, ms between keys, ms a frame) {label:?}: the legs ran in place \
             against the chip (from ms, ms, changes of their art, col, row, glyph) {worst:?}"
        );
        assert!(
            *past && home.is_some_and(|(c, r)| c < f32::from(label.0) && (r - 20.0).abs() < 0.5),
            "fixture {label:?}: the cat went out past the chip with the writing and is back by \
             the prompt at the end ({home:?})"
        );
    }
}

/// How far (cells) past the end of what aterm painted on Claude Code's footer rows, or past the
/// caret's column, the resting cat's left edge may sit
/// ([`the_cat_rests_by_the_words_the_footer_shows`]).
const FOOTER_SEAT_REACH: f32 = 2.0;

/// THE CAT RESTS BY THE WORDS THE FOOTER SHOWS (the owner's screenshot, 2026-10-27: the cat
/// floating far right of `YOLO on · 1 shell` while the caret sat at the input row's left).
/// Claude Code works on its turn and the cat rests beside its footer, under the composer. aterm
/// rewrites the mode row there to its kept items and blanks the rest of the row to the end of
/// Claude's own words, and the row under it whole ([`Knobs::footer_chrome`]); the pet looks at
/// Claude's cells before aterm paints over them. So it reads the glass aterm blanks as the
/// blank the person sees ([`PetReserve::blanked`]) and the words aterm laid as the host's
/// chrome: at rest in the footer its left edge sits within [`FOOTER_SEAT_REACH`] of the end of
/// those words, or of the caret's column, never out past them in glass that shows nothing, and
/// its settled body never lies over the words. Judged with the words a job leaves
/// ([`MODE_ROW_WORDS`]) and with none, the row Claude Code draws with no job blanked whole.
/// NEGATIVE CONTROLS (as run): with the host handing the rewrite as it did before, its whole
/// extent protected and nothing blanked ([`Knobs::footer_whole`]), the cat rested 60.4 columns
/// right of the caret in the first take, 64.4 in the second, past Claude's hidden letters, in
/// every run; with the pet reading `blanked` as nothing, the same; with the blank left where its
/// rows were when the composer moves under the chrome a frame late ([`Knobs::chrome_lag`]),
/// rather than carried with them as the chrome is, the same in every lagging run with words;
/// with the host blanking its own words too, none of them protected, the cat sat over them from
/// the first frames to the end in nine of the twelve runs with words.
#[test]
fn the_cat_rests_by_the_words_the_footer_shows() {
    let first = BUSY_FOOTER_REST;
    let takes = [
        first,
        BusyClaude {
            cols: 120,
            rows: 40,
            feet: (15, 66),
            ..first
        },
        BusyClaude {
            whole: true,
            hide: false,
            lines: (900, 7),
            ..first
        },
        BusyClaude {
            gap: 0,
            spin: 40,
            lines: (150, 33),
            ..first
        },
    ];
    let runs: Vec<(usize, u64, bool, u16)> = (0..takes.len())
        .flat_map(|take| {
            [(8, false), (16, true), (8, true)]
                .into_iter()
                .flat_map(move |(ms, lag)| {
                    [MODE_ROW_WORDS, 0].map(|footer_words| (take, ms, lag, footer_words))
                })
        })
        .collect();
    let seen = crate::common::par_map(&runs, |&(take, frame_ms, chrome_lag, footer_words)| {
        let b = takes[take];
        let window = Take {
            name: "busy",
            src: "",
            rows: b.rows as u16,
            cols: b.cols as u16,
        };
        let knobs = Knobs {
            chips: true,
            brand: true,
            footer_chrome: true,
            footer_words,
            frame_ms,
            chrome_lag,
            ..Knobs::SHIPPED
        };
        let h = Host::replay_events(&window, knobs, busy_claude(b, 15));
        let words = f32::from(footer_words).min(b.feet.0.min(b.cols - 1) as f32);
        let mut rest = 0;
        let mut worst: Option<(u64, f32, f32, (u16, u16))> = None;
        // The first present the settled cat's body lay over the words aterm laid on the mode
        // row, (ms, its left edge, its feet's row), and how long it lay there in all (ms).
        let mut over: Option<(u64, f32, f32)> = None;
        let mut over_ms = 0;
        for s in &h.shots {
            let in_footer = s.rule.is_some_and(|r| s.frame.row.round() as usize > r);
            if s.frame.alpha == 0 || gait(&s.frame).is_some() || !in_footer {
                continue;
            }
            rest += frame_ms;
            let past = s.frame.col - words.max(f32::from(s.caret.1));
            if past > FOOTER_SEAT_REACH && worst.is_none_or(|w| past > w.1) {
                worst = Some((s.ms, past, s.frame.col, s.caret));
            }
            let (x0, _, y0, y1) = s
                .frame
                .body_px(CW, CH, window.cols, window.rows)
                .expect("a drawn cat has a body");
            let mode_row = s.rule.map_or(0, |r| r + 1) as i32 * i32::from(CH);
            let left = x0 as f32 / f32::from(CW);
            let on_mode_row = y0 < mode_row + i32::from(CH) && y1 > mode_row;
            if s.frame.action.settled() && on_mode_row && left < words {
                over.get_or_insert((s.ms, left, s.frame.row));
                over_ms += frame_ms;
            }
        }
        let over = over.map(|o| (o, over_ms));
        (
            (take, frame_ms, chrome_lag, footer_words),
            rest,
            worst,
            over,
        )
    });
    for (label, rest, _, _) in &seen {
        assert!(
            *rest >= 5_000,
            "fixture (take, ms a frame, chrome lag, words) {label:?}: the cat rests beside the \
             host's footer under the composer ({rest} ms)"
        );
    }
    let past: Vec<_> = seen
        .iter()
        .filter_map(|(label, _, worst, _)| worst.map(|w| (label, w)))
        .collect();
    assert!(
        past.is_empty(),
        "the cat rested in the footer past the words aterm shows there ((take, ms a frame, \
         chrome lag, words), (ms, cells past them, col, caret)): {past:?}"
    );
    let over: Vec<_> = seen
        .iter()
        .filter_map(|(label, _, _, over)| over.map(|o| (label, o)))
        .collect();
    assert!(
        over.is_empty(),
        "the cat rested over the words aterm lays on the mode row ((take, ms a frame, chrome \
         lag, words), ((first ms, body's left edge, row), ms in all)): {over:?}"
    );
}
