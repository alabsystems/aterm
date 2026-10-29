// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! THE HEAD LAW, end to end: an Enter at the bottom row scrolls the command's echo and its output
//! up over the new prompt, and the 1.7-row cat beside the caret reaches 0.7 of a row into the line
//! above it.

use std::time::{Duration, Instant};

use aterm_core::render::RenderInput;
use aterm_core::terminal::{ClockReading, Terminal};
use aterm_effects::color_math::contrast_ratio;
use aterm_effects::companion::{CompanionOwner, GlowOwnership, PetFacts};
use aterm_effects::cursor_glow::GlowStyle;
use aterm_effects::host::{FrameGeom, HostFrameInput, TerminalFacts};
use aterm_effects::kitty_pet::{PetAction, PetFrame, PetInputKind};
use aterm_effects::pet_world::{PetPane, PetReserve, PetWorldFacts};
use aterm_effects::word_decorations::{DecoConfig, WordDecorations};
use aterm_lexicon::Lexicon;

use crate::common::{FrameHold, Rec};

const ROWS: u16 = 14;
const COLS: u16 = 72;
const CELL_W: u16 = 14;
const CELL_H: u16 = 26;
const FRAME_MS: u64 = 16;
/// WCAG's bar for text.
const BAR: f32 = 4.5;
/// The tile's transparent border on each side, cells: no fur stands there
/// (the brain's own footprint reading shaves the same, `kitty_pet`'s
/// `INK_PAD`).
const TILE_BORDER: f32 = 0.30;

/// A page and its ink.
#[derive(Clone, Copy, Debug)]
struct Theme {
    name: &'static str,
    page: u32,
    ink: u32,
}

const DARK: Theme = Theme {
    name: "dark",
    page: 0x000000,
    ink: 0xFFFFFF,
};
const LIGHT: Theme = Theme {
    name: "light",
    page: 0xFFFFFF,
    ink: 0x000000,
};

/// One presented frame: the cat, and the worst letter under it.
#[derive(Clone, Copy, Debug)]
struct Shot {
    ms: u64,
    frame: PetFrame,
    /// The least worst-case contrast of a letter the body lies on, and
    /// where (`None`: no letter under it).
    worst: Option<(f32, u16, u16)>,
}

struct Host {
    term: Terminal,
    theme: Theme,
    owner: CompanionOwner,
    decos: WordDecorations,
    lexicon: Lexicon,
    cfg: DecoConfig,
    t0: Instant,
    now: Instant,
    vsync: Instant,
    hold: FrameHold,
    shots: Vec<Shot>,
    input: Option<(u64, RenderInput)>,
}

/// `a` over `b` at `alpha`, channel by channel.
fn over(a: u32, b: u32, alpha: f32) -> u32 {
    let ch = |sh: u32| {
        let (x, y) = (((a >> sh) & 0xff) as f32, ((b >> sh) & 0xff) as f32);
        ((x * alpha + y * (1.0 - alpha)).round() as u32).min(255)
    };
    (ch(16) << 16) | (ch(8) << 8) | ch(0)
}

impl Host {
    fn new(seed: u64, theme: Theme, setup: &dyn Fn(&mut CompanionOwner)) -> Self {
        let mut term = Terminal::new(ROWS, COLS);
        term.process(
            format!(
                "\x1b]11;#{:06x}\x07\x1b]10;#{:06x}\x07",
                theme.page, theme.ink
            )
            .as_bytes(),
        );
        let mut owner = CompanionOwner::default();
        owner.set_enabled_seed(true, seed);
        setup(&mut owner);
        let now = Instant::now();
        Self {
            term,
            theme,
            owner,
            decos: WordDecorations::default(),
            lexicon: Lexicon::with_languages(&["en"]),
            cfg: DecoConfig::default(),
            t0: now,
            now,
            vsync: now + Duration::from_millis(FRAME_MS),
            hold: FrameHold::default(),
            shots: Vec::new(),
            input: None,
        }
    }

    fn ms(&self) -> u64 {
        self.now.saturating_duration_since(self.t0).as_millis() as u64
    }

    /// The least worst-case contrast among the letters the body's box lies
    /// on: every letter cell whose row the box reaches by a quarter of a cell
    /// or more (less is the descender band, under the letters' bodies) and
    /// whose column the box, shaved of the tile's transparent border
    /// ([`TILE_BORDER`]), reaches at all.
    fn worst_under(&self, input: &RenderInput, frame: &PetFrame) -> Option<(f32, u16, u16)> {
        let (x0, x1, y0, y1) = frame.body_px(CELL_W, CELL_H, COLS, ROWS)?;
        let border = (TILE_BORDER * f32::from(CELL_W)).round() as i32;
        let (x0, x1) = (x0 + border, x1 - border);
        let alpha = f32::from(frame.alpha) / 255.0;
        let ground = over(self.theme.ink, self.theme.page, alpha);
        let bound = contrast_ratio(self.theme.ink, ground);
        let (cw, ch) = (i32::from(CELL_W), i32::from(CELL_H));
        let mut worst: Option<(f32, u16, u16)> = None;
        for row in y0.div_euclid(ch).max(0)..=((y1 - 1).div_euclid(ch)).min(i32::from(ROWS) - 1) {
            let reach = ((row + 1) * ch).min(y1) - (row * ch).max(y0);
            if reach * 4 < ch {
                continue;
            }
            let cells = &input.cells[row as usize];
            for col in x0.div_euclid(cw).max(0)..=((x1 - 1).div_euclid(cw)).min(i32::from(COLS) - 1)
            {
                let letter = cells
                    .get(col as usize)
                    .is_some_and(|c| c.ch.is_alphanumeric());
                if letter && worst.is_none_or(|(w, ..)| bound < w) {
                    worst = Some((bound, row as u16, col as u16));
                }
            }
        }
        worst
    }

    fn present(&mut self) {
        if self.hold.withholds(&self.term, self.now) {
            return;
        }
        let mutations = self.term.screen_mutations();
        if self.input.as_ref().is_none_or(|(at, _)| *at != mutations) {
            let fresh = self.term.cell_frame(usize::from(ROWS), usize::from(COLS));
            self.input = Some((mutations, fresh));
        }
        let (_, input) = self.input.take().expect("snapshot taken above");
        let world = PetWorldFacts::read(&self.term, 7);
        let reserve = PetReserve {
            protected: &[],
            overlays: &[],
            ink: &[],
            blanked: &[],
            painted_over: None,
        };
        self.owner
            .observe_console_reserved(&input, &world, PetPane::full(&input), reserve);
        let facts = TerminalFacts::read(&self.term, 7, false);
        let host = HostFrameInput::new(
            self.now,
            FrameGeom {
                rows: ROWS,
                cols: COLS,
                cell_w: CELL_W,
                cell_h: CELL_H,
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
            usize::from(ROWS),
            usize::from(COLS),
            &self.lexicon,
            &self.cfg,
            input.content_seq,
            self.now,
        );
        let worst = self.worst_under(&input, &frame);
        self.input = Some((mutations, input));
        self.shots.push(Shot {
            ms: self.ms(),
            frame,
            worst,
        });
    }

    fn run_to(&mut self, ms: u64) {
        let t = self.t0 + Duration::from_millis(ms);
        while self.vsync <= t {
            self.now = self.vsync;
            self.present();
            self.vsync += Duration::from_millis(FRAME_MS);
        }
        self.now = self.now.max(t);
    }

    fn replay(
        theme: Theme,
        unseen: bool,
        events: &[(u64, Rec, Option<PetInputKind>)],
        tail: u64,
    ) -> Self {
        Self::replay_with(
            theme,
            &|owner| owner.set_pet_letters_unseen(unseen),
            events,
            tail,
        )
    }

    /// [`Self::replay`] with the owner set up by `setup` first.
    fn replay_with(
        theme: Theme,
        setup: &dyn Fn(&mut CompanionOwner),
        events: &[(u64, Rec, Option<PetInputKind>)],
        tail: u64,
    ) -> Self {
        let mut h = Self::new(0x5EED, theme, setup);
        for (ms, ev, kind) in events {
            h.run_to(*ms);
            if let Some(kind) = kind {
                h.owner.note_console_input(h.now, *kind);
            }
            if let Rec::Out(b) = ev {
                let at = ClockReading {
                    monotonic: h.now,
                    wall_ms: None,
                };
                h.term.process_at(b, at);
            }
        }
        let end = h.ms() + tail;
        h.run_to(end);
        h
    }
}

/// `seq 1 40` on a 14-row screen, the prompt `$ ` on the bottom row, the
/// command `typed` at 60 ms a key, then Enter: the command's line and the
/// line it prints scroll up and the new prompt is on the bottom row. The
/// events, and when Enter was pressed.
fn enter_bottom(typed: &str, printed: &str) -> (Vec<(u64, Rec, Option<PetInputKind>)>, u64) {
    let mut ev = Vec::new();
    let mut seq = String::from("\x1b[2J\x1b[H");
    for i in 1..=40 {
        seq.push_str(&format!("{i}\r\n"));
    }
    seq.push_str("$ ");
    ev.push((300, Rec::Out(seq.into_bytes()), None));
    let mut t = 1_000u64;
    for c in typed.bytes() {
        ev.push((t, Rec::In(vec![c]), Some(PetInputKind::Text)));
        ev.push((t + 2, Rec::Out(vec![c]), None));
        t += 60;
    }
    t += 45;
    let enter = t;
    ev.push((t, Rec::In(vec![b'\r']), Some(PetInputKind::Submit)));
    ev.push((t + 2, Rec::Out(b"\r\n".to_vec()), None));
    ev.push((
        t + 6,
        Rec::Out(format!("{printed}\r\n$ ").into_bytes()),
        None,
    ));
    (ev, enter)
}

/// The frames from Enter on.
fn after(h: &Host, enter: u64) -> impl Iterator<Item = &Shot> {
    h.shots.iter().filter(move |s| s.ms >= enter)
}

/// How many times the cat's opacity came back up after going down: a body
/// that fades and returns reads as blinking.
fn returns(shots: &[&Shot]) -> usize {
    let mut fell = false;
    let mut back = 0;
    for w in shots.windows(2) {
        let (a, b) = (w[0].frame.alpha, w[1].frame.alpha);
        if b < a {
            fell = true;
        } else if b > a && fell {
            back += 1;
            fell = false;
        }
    }
    back
}

/// A short line printed by an Enter at the bottom row: the cat rests beside
/// it, its whole body on blank cells, and the bound there flies under no
/// letter's body. Its body stays whole throughout (no fade, no return), so
/// every letter near it keeps the theme's full contrast. The negative
/// control (the letter reader blinded) rests on the printed words, under the
/// bar, on both themes.
#[test]
fn a_cat_rests_beside_the_line_an_enter_at_the_bottom_prints() {
    let (ev, enter) = enter_bottom("echo the quick brown fox", "the quick brown fox");
    for theme in [DARK, LIGHT] {
        let h = Host::replay(theme, false, &ev, 3_000);
        let shots: Vec<&Shot> = after(&h, enter).collect();
        for s in &shots {
            if let Some((c, row, col)) = s.worst {
                assert!(
                    c >= BAR,
                    "{}: {} ms after Enter the cat ({:?}, alpha {}) lies on the letter at \
                     ({row}, {col}), {c:.2}:1 at worst",
                    theme.name,
                    s.ms - enter,
                    s.frame.action,
                    s.frame.alpha,
                );
            }
        }
        let last = shots.last().expect("frames after Enter");
        assert!(
            last.frame.action.settled() && last.frame.alpha == 255,
            "{}: the cat ends at rest and whole ({:?}, alpha {})",
            theme.name,
            last.frame.action,
            last.frame.alpha
        );
        assert!(
            shots.iter().all(|s| s.frame.alpha == 255),
            "{}: nothing veils a cat that rests off the words",
            theme.name
        );

        let blind = Host::replay(theme, true, &ev, 3_000);
        let worst = after(&blind, enter)
            .filter(|s| s.frame.action.settled())
            .filter_map(|s| s.worst)
            .map(|(c, ..)| c)
            .fold(f32::INFINITY, f32::min);
        assert!(
            worst < BAR,
            "{}: the negative control rests on the printed words ({worst:.2}:1); the law \
             above reads nothing",
            theme.name
        );
    }
}

/// Each flight, as `(ms its gather began, the presence it gathered with, the
/// presence it left the ground with, the least and the most it was drawn at
/// from its gather to its last landing frame)`.
fn flights(shots: &[&Shot]) -> Vec<(u64, u8, u8, u8, u8)> {
    let mut out = Vec::new();
    let mut i = 1;
    while i < shots.len() {
        if shots[i].frame.action != PetAction::Leap || shots[i - 1].frame.action == PetAction::Leap
        {
            i += 1;
            continue;
        }
        let mut g = i;
        while g > 1 && shots[g - 1].frame.action == PetAction::Crouch {
            g -= 1;
        }
        let gathered = shots[g - 1].frame.alpha;
        let left = shots[i - 1].frame.alpha;
        let mut end = i;
        while end + 1 < shots.len()
            && matches!(
                shots[end + 1].frame.action,
                PetAction::Leap | PetAction::Land
            )
        {
            end += 1;
        }
        let span = &shots[g..=end];
        let least = span.iter().map(|s| s.frame.alpha).min().unwrap_or(0);
        let most = span.iter().map(|s| s.frame.alpha).max().unwrap_or(0);
        out.push((shots[g].ms, gathered, left, least, most));
        i = end + 1;
    }
    out
}

/// A line lettered to the margin leaves no ground beside it: the cat rests
/// where it would, and the rest veil takes its body under the letters' bar
/// once and holds it there. It never comes back while it lies on them (a
/// body that dips and returns blinks), through a program's moves of the
/// caret along the row, a step, a hop and a bound away and back: a flight
/// keeps the presence it left with, from its gather to its landing, never
/// above it and never under it. Judged at every frame from the Enter: no
/// letter under the cat reads under the bar in any of them (the veil steps
/// on in the frame that scrolls the letters in under the resting cat), and
/// the bound to the new prompt gathers on the letters for a landing on them,
/// so the flight leaves veiled. Negative controls: with the letters unseen,
/// the cat rests on them under the bar; the rest veil dwelt on and eased in
/// ([`CompanionOwner::set_pet_letter_arrival_dwells`]) lays a letter under
/// the bar in at least 20 frames after the Enter.
#[test]
fn a_cat_under_a_line_lettered_to_the_margin_is_veiled_once() {
    let words: String = "lorem ipsum dolor sit amet consectetur adipiscing elit sed do eiusmod"
        .chars()
        .chain(std::iter::repeat('x'))
        .take(usize::from(COLS))
        .collect();
    let (mut ev, enter) = enter_bottom("cat lorem.txt", &words);
    for (k, col) in [9u16, 17, 48, 3].into_iter().enumerate() {
        let at = enter + 2_500 * (k as u64 + 1);
        ev.push((at, Rec::Out(format!("\x1b[{col}G").into_bytes()), None));
    }
    for theme in [DARK, LIGHT] {
        let h = Host::replay(theme, false, &ev, 3_000);
        let shots: Vec<&Shot> = after(&h, enter).collect();
        let under: Vec<_> = shots
            .iter()
            .filter(|s| s.worst.is_some_and(|(c, ..)| c < BAR))
            .map(|s| (s.ms - enter, s.frame.alpha, s.worst))
            .collect();
        assert!(
            under.is_empty(),
            "{}: frames after Enter lay a letter under the bar (ms after Enter, alpha, \
             worst): {under:?}",
            theme.name
        );
        let settled = shots.iter().filter(|s| s.frame.action.settled()).count();
        assert!(
            settled > 300,
            "{}: the cat rests between the moves ({settled} frames)",
            theme.name
        );
        let on_letters: Vec<&Shot> = shots
            .iter()
            .copied()
            .filter(|s| s.worst.is_some())
            .collect();
        assert_eq!(
            returns(&on_letters),
            0,
            "{}: a cat on the letters fades once and stays faded",
            theme.name
        );
        let flown = flights(&shots);
        assert!(
            flown.len() >= 3,
            "{}: the moves fly ({flown:?})",
            theme.name
        );
        for &(ms, gathered, left, least, most) in &flown {
            assert!(
                most <= gathered && u32::from(least) * 10 >= u32::from(left) * 9,
                "{}: the flight gathered {} ms after Enter at {gathered} left the ground at \
                 {left} and was drawn from {least} to {most}",
                theme.name,
                ms - enter
            );
        }

        let blind = Host::replay(theme, true, &ev, 3_000);
        let last = after(&blind, enter).last().expect("frames after Enter");
        assert!(
            last.worst.is_some_and(|(c, ..)| c < BAR),
            "{}: the negative control rests on the letters at full body ({:?}); the law \
             above reads nothing",
            theme.name,
            last.worst
        );
        let dwelt = Host::replay_with(
            theme,
            &|owner| owner.set_pet_letter_arrival_dwells(true),
            &ev,
            3_000,
        );
        let washed = after(&dwelt, enter)
            .filter(|s| s.worst.is_some_and(|(c, ..)| c < BAR))
            .count();
        assert!(
            washed >= 20,
            "{}: the negative control (the rest veil dwelt on and eased in) lays a letter \
             under the bar in only {washed} frames after Enter",
            theme.name
        );
    }
}

/// Where the body's silhouette (its drawn box, shaved of the tile's
/// transparent border at the sides) meets a cell with a glyph on it, by any
/// part of a pixel row: the first such cell.
fn meets_glyphs(input: &RenderInput, frame: &PetFrame) -> Option<(u16, u16)> {
    let (x0, x1, y0, y1) = frame.body_px(CELL_W, CELL_H, COLS, ROWS)?;
    let border = (TILE_BORDER * f32::from(CELL_W)).round() as i32;
    let (x0, x1) = (x0 + border, x1 - border);
    let (cw, ch) = (i32::from(CELL_W), i32::from(CELL_H));
    for row in y0.div_euclid(ch).max(0)..=((y1 - 1).div_euclid(ch)).min(i32::from(ROWS) - 1) {
        for col in x0.div_euclid(cw).max(0)..=((x1 - 1).div_euclid(cw)).min(i32::from(COLS) - 1) {
            let glyph = input.cells[row as usize]
                .get(col as usize)
                .is_some_and(|c| c.ch != ' ' && c.ch != '\0');
            if glyph {
                return Some((row as u16, col as u16));
            }
        }
    }
    None
}

/// THE HEAD LAW AT THE MARGIN, end to end: the trail gallery's `burst` (a
/// line typed at 50 ms a key to three cells short of the margin, then quiet).
/// The cat waits at the wall, the line's last letters running over it, while
/// the hand may still write on and wrap; it never leaves on a guess of a wrap
/// that never comes. Once the hand has stopped (the cat sits, `SIT_AFTER`
/// after the last key) it hops off the letters to the nearest seat by the
/// caret that covers none, and rests there: every rest frame from then on
/// stands off every glyph, on dark and on paper, the whole body on blank rows,
/// and it is back on the ground within `SETTLED_BY` of the last key. The
/// negative control (the letter reader blinded: the stand as it was) rests on
/// the line among its letters.
#[test]
fn a_cat_waiting_on_a_wrap_that_never_comes_rests_off_the_line() {
    /// The cat's sit after the hand's last key (`kitty_pet::SIT_AFTER`).
    const SIT_AFTER: u64 = 1_400;
    /// The sit, the hop off the letters, and its landing.
    const SETTLED_BY: u64 = SIT_AFTER + 600;
    let text = "echo the quick brown fox jumps over the lazy dog as the kitty flies";
    let mut ev = vec![(300u64, Rec::Out(b"\x1b[2J\x1b[5H$ ".to_vec()), None)];
    let mut t = 1_000u64;
    for c in text.bytes() {
        ev.push((t, Rec::In(vec![c]), Some(PetInputKind::Text)));
        ev.push((t + 2, Rec::Out(vec![c]), None));
        t += 50;
    }
    let quiet = t;
    let line =
        |h: &mut Host| -> RenderInput { h.term.cell_frame(usize::from(ROWS), usize::from(COLS)) };
    for theme in [DARK, LIGHT] {
        let mut h = Host::replay(theme, false, &ev, 4_600);
        let input = line(&mut h);
        let rests: Vec<&Shot> = h
            .shots
            .iter()
            .filter(|s| s.ms >= quiet + SETTLED_BY && s.frame.action.settled() && s.frame.alpha > 0)
            .collect();
        assert!(
            rests.len() > 100,
            "{}: the cat rests after the burst ({} frames)",
            theme.name,
            rests.len()
        );
        let aloft: Vec<u64> = h
            .shots
            .iter()
            .filter(|s| s.ms >= quiet && s.frame.action == PetAction::Leap)
            .map(|s| s.ms - quiet)
            .collect();
        assert!(
            aloft.iter().all(|&ms| ms >= SIT_AFTER - 100),
            "{}: the cat leaves the wall only once the hand has stopped, in the air at {:?} ms",
            theme.name,
            aloft
        );
        for s in &rests {
            assert_eq!(
                meets_glyphs(&input, &s.frame),
                None,
                "{}: {} ms after the burst the resting cat ({:?} at col {:.2} row {:.2}, box \
                 {:?}) meets a glyph",
                theme.name,
                s.ms - quiet,
                s.frame.action,
                s.frame.col,
                s.frame.row,
                s.frame.body_px(CELL_W, CELL_H, COLS, ROWS)
            );
        }

        let mut blind = Host::replay(theme, true, &ev, 4_600);
        let input = line(&mut blind);
        let last = blind.shots.last().expect("frames");
        assert!(
            last.frame.action.settled()
                && meets_glyphs(&input, &last.frame).is_some_and(|(row, _)| row == 4),
            "{}: the negative control rests with its head among the line's letters ({:?} at \
             col {:.2} row {:.2})",
            theme.name,
            last.frame.action,
            last.frame.col,
            last.frame.row
        );
    }
}

/// THE ENTER'S STEP FIRES ONCE: output that keeps streaming in under a
/// resting cat after the hand's last key, an Enter, is no Enter's letters.
/// From two seconds after the Enter, lines that alternate between lettered
/// to the margin and a lone `.` scroll up under the body eight a second.
/// Judged from the stream's first line: the cat's presence
/// never falls more than 60 levels in one frame and never comes back up
/// once it fell (a body that dips and returns blinks), and no letter under
/// it reads under the bar (the veil the Enter's letters stepped on holds
/// while they come and go within the dwell). Negative control: the step left
/// armed while the Enter was the last key
/// ([`CompanionOwner::set_pet_letter_step_rearms`]) drops the veil in one
/// frame each time the letters come back after it eased up.
#[test]
fn output_streaming_after_an_enter_never_blinks_the_veiled_cat() {
    let words: String = "lorem ipsum dolor sit amet consectetur adipiscing elit sed do eiusmod"
        .chars()
        .chain(std::iter::repeat('x'))
        .take(usize::from(COLS))
        .collect();
    let (mut ev, enter) = enter_bottom("cat lorem.txt", &words);
    let start = enter + 2_000;
    for k in 0..24u64 {
        let line = if k % 2 == 0 { words.as_str() } else { "." };
        ev.push((
            start + 125 * k,
            Rec::Out(format!("\r\x1b[K{line}\r\n$ ").into_bytes()),
            None,
        ));
    }
    let end = start + 125 * 24;
    let judge = |h: &Host| -> (usize, usize, usize, usize) {
        let span: Vec<&Shot> = h
            .shots
            .iter()
            .filter(|s| s.ms >= start && s.ms <= end)
            .collect();
        let settled = span.iter().filter(|s| s.frame.action.settled()).count();
        let falls = span
            .windows(2)
            .filter(|w| u32::from(w[1].frame.alpha) + 60 < u32::from(w[0].frame.alpha))
            .count();
        let under = span
            .iter()
            .filter(|s| s.worst.is_some_and(|(c, ..)| c < BAR))
            .count();
        (settled, returns(&span), falls, under)
    };
    for theme in [DARK, LIGHT] {
        let h = Host::replay(theme, false, &ev, 1_000);
        let (settled, back, falls, under) = judge(&h);
        assert!(
            settled > 150,
            "{}: fixture: the cat rests through the stream ({settled} frames)",
            theme.name
        );
        assert_eq!(
            (back, falls, under),
            (0, 0, 0),
            "{}: through the stream the cat came back {back} times, fell over 60 levels in one \
             frame {falls} times, and lay on a letter under the bar in {under} frames",
            theme.name
        );
        let armed = Host::replay_with(
            theme,
            &|owner| owner.set_pet_letter_step_rearms(true),
            &ev,
            1_000,
        );
        let (_, a_back, a_falls, _) = judge(&armed);
        assert!(
            a_back >= 5 && a_falls >= 5,
            "{}: the negative control (the step left armed) came back {a_back} times and fell \
             in one frame {a_falls} times",
            theme.name
        );
    }
}
