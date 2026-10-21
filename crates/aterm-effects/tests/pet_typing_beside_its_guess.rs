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
}

impl Knobs {
    /// The owner's host: the guess as ink, 120 Hz, the recorded echo, no chips.
    const SHIPPED: Self = Self {
        reserve: Reserve::Ink,
        frame_ms: 8,
        echo_ms: 0,
        chips: false,
    };
}

/// The footer chips' width: `◆ Opus 5.5 xhigh ⌂ ~/aterm ⎇ main Σ $1.23` and
/// their gaps, as the owner's footer writes them at 120 columns.
const CHIPS_COLS: u16 = 57;

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

    /// The composer's bottom rule: the last row drawn mostly in U+2500 (at
    /// least four fifths of the width), below another such row.
    fn bottom_rule(&self, input: &RenderInput) -> Option<usize> {
        let cols = usize::from(self.cols);
        let rules: Vec<usize> = input
            .cells
            .iter()
            .enumerate()
            .filter(|(_, row)| row.iter().filter(|c| c.ch == '─').count() * 5 >= cols * 4)
            .map(|(r, _)| r)
            .collect();
        (rules.len() >= 2).then(|| rules[rules.len() - 1])
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
        let rule = self.bottom_rule(input);
        let chips: Vec<PetRect> = rule
            .filter(|_| self.knobs.chips)
            .map(|row| {
                let cols = f32::from(self.cols);
                let w = f32::from(CHIPS_COLS);
                PetRect::new(row as f32, cols - 1.0 - w, 1.0, w)
            })
            .into_iter()
            .collect();
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
        let mut h = Self::new(take, knobs);
        let mut events: Vec<(u64, Rec)> = Vec::new();
        for (ms, ev) in recording(take.src, 1_000_000) {
            match ev {
                Rec::In(b) if b.as_slice() == b"\x1b[99;5u" => {
                    events.push((ms, Rec::In(b)));
                    break;
                }
                Rec::In(b) => events.push((ms, Rec::In(b))),
                Rec::Out(b) => events.push((ms + knobs.echo_ms, Rec::Out(b))),
            }
        }
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
    /// Runs of presents drawing the run's art while the body creeps (under
    /// [`CREEP_SPEED`] over three presents): `(ms it began, ms it lasted)`.
    creeps: Vec<(u64, u64)>,
}

/// The gait art a frame draws: the walk's, the amble's or the run's cycle.
fn gait(frame: &PetFrame) -> Option<&'static str> {
    let id = aterm_effects::pet_glyphs_gen::PET_GLYPHS[frame.pose as usize].id;
    ["pet_walk_", "pet_lwalk_", "pet_run_"]
        .into_iter()
        .find(|family| id.starts_with(family))
}

/// Under this speed (cells/s) a body drawn in the run's art creeps.
const CREEP_SPEED: f32 = 3.0;
/// The longest a run's art may creep: a gallop stalled between two late
/// keys, not a slow-motion gallop.
const CREEP_MAX_MS: u64 = 130;

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
    let mut creep_from = None;
    for w in shots.windows(4) {
        let s = &w[3];
        let speed = (s.frame.col - w[0].frame.col).abs() / (3.0 * dt);
        let creeping = gait(&s.frame) == Some("pet_run_") && speed < CREEP_SPEED;
        match (creeping, creep_from) {
            (true, None) => creep_from = Some(s.ms),
            (false, Some(from)) => {
                m.creeps.push((from, s.ms - from));
                creep_from = None;
            }
            _ => {}
        }
    }
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
/// present goes back to a gait's art within 350 ms of last drawing it, and the
/// run's art never creeps for longer than [`CREEP_MAX_MS`].
#[test]
fn the_legs_keep_one_gait_while_typing() {
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
            creep.is_none_or(|c| c.1 <= CREEP_MAX_MS),
            "{label}: the run's art crept under {CREEP_SPEED} cells/s for {creep:?} ms \
             (ms it began, ms it lasted)"
        );
    }
}

/// The composer grows a line under a cat that rests beside aterm's footer
/// chips. Typing a long prompt in Claude Code with the chips on (the owner's
/// footer), the cat rests under the composer, its head beside the chips'
/// rule; at each wrap the composer grows a line, and the rule and the chips
/// come down onto the cat's head. The console law stepped it aside: in one
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
                m.rides.len() >= 2,
                "{label}: each wrap grows the composer under the cat: rides {:?}",
                m.rides
            );
        }
    }
}

/// A wrap typed through is one bound. At the composer's right edge the caret
/// wraps to the next line and the person types on; the cat bounds the width
/// of the composer to it. The bound was aimed once, at launch, and the hand
/// typed some ten cells on through its second in the air: the cat came down
/// nine cells behind the caret, and the jump it had latched at the wrap
/// launched it again (2026-10-20, review of `fix/kitty-jitter`: about 3 s of
/// leaping at every wrap). The landing now tracks the writing in the air,
/// and the flight spends the latch it was launched for: after each wrap,
/// while the caret stays on the new line, one flight, and it comes down ahead
/// of the caret.
#[test]
fn a_wrap_typed_through_is_one_bound() {
    let h = Host::replay(&TAKES[0], Knobs::SHIPPED);
    let wraps: Vec<usize> = h
        .shots
        .windows(2)
        .enumerate()
        .filter(|(_, w)| {
            w[1].caret.0 == w[0].caret.0 + 1 && w[0].caret.1 + 10 >= h.cols && w[1].caret.1 < 10
        })
        .map(|(i, _)| i + 1)
        .collect();
    assert!(wraps.len() >= 2, "the take must wrap twice: {wraps:?}");
    for &at in &wraps {
        // From the wrap for three seconds, while the caret stays on the line
        // it wrapped to (a submit after it sends the cat elsewhere).
        let (start, line) = (h.shots[at].ms, h.shots[at].caret.0);
        let after: Vec<&Shot> = h
            .shots
            .iter()
            .skip(at)
            .take_while(|s| s.ms <= start + 3_000 && s.caret.0 == line)
            .collect();
        let launches: Vec<(u64, f32)> = after
            .windows(2)
            .filter(|w| {
                w[1].frame.action == PetAction::Leap && w[0].frame.action != PetAction::Leap
            })
            .map(|w| (w[1].ms, w[1].frame.col))
            .collect();
        assert_eq!(
            launches.len(),
            1,
            "the wrap at {start} ms: flights (ms, col) {launches:?}"
        );
        let landed = after
            .windows(2)
            .find(|w| w[0].frame.action == PetAction::Leap && w[1].frame.action != PetAction::Leap)
            .map(|w| w[1])
            .expect("the bound lands");
        assert!(
            landed.frame.col >= f32::from(landed.caret.1),
            "the wrap at {start} ms: the bound came down at col {:.2}, behind the caret at {:?}",
            landed.frame.col,
            landed.caret
        );
    }
}
