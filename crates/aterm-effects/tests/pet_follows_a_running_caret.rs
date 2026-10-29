// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Tests that the companion pet follows a running caret and crosses soft wraps in one flight.

use std::time::{Duration, Instant};

use aterm_core::render::RenderInput;
use aterm_core::terminal::{ClockReading, Terminal};
use aterm_effects::companion::{CompanionOwner, GlowOwnership, PetFacts};
use aterm_effects::cursor_glow::GlowStyle;
use aterm_effects::host::{FrameGeom, HostFrameInput, TerminalFacts};
use aterm_effects::kitty_pet::{PetAction, PetFrame, PetInputKind};
use aterm_effects::pet_world::{PetPane, PetReserve, PetWorldFacts};
use aterm_effects::word_decorations::{DecoConfig, WordDecorations};
use aterm_lexicon::Lexicon;

use crate::common::{FrameHold, Rec};

const ROWS: u16 = 24;
const COLS: u16 = 80;
const FRAME_MS: u64 = 16;
const PROMPT: i32 = 2;

/// One present, as the pet drew it, and the caret on it.
#[derive(Clone, Copy, Debug)]
struct Shot {
    ms: u64,
    frame: PetFrame,
    caret: (u16, u16),
}

struct Host {
    term: Terminal,
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

impl Host {
    fn new() -> Self {
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

    fn present(&mut self) {
        if self.hold.withholds(&self.term, self.now) {
            return;
        }
        let mutations = self.term.screen_mutations();
        if self.input.as_ref().is_none_or(|(at, _)| *at != mutations) {
            let fresh = self.term.cell_frame(usize::from(ROWS), usize::from(COLS));
            self.input = Some((mutations, fresh));
        }
        let (_, input) = self.input.as_ref().expect("snapshot taken above");
        let world = PetWorldFacts::read(&self.term, 7);
        let reserve = PetReserve {
            protected: &[],
            overlays: &[],
            ink: &[],
            blanked: &[],
            painted_over: None,
        };
        self.owner
            .observe_console_reserved(input, &world, PetPane::full(input), reserve);
        let facts = TerminalFacts::read(&self.term, 7, false);
        let host = HostFrameInput::new(
            self.now,
            FrameGeom {
                rows: ROWS,
                cols: COLS,
                cell_w: 15,
                cell_h: 28,
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
        let caret = self.term.cursor();
        self.shots.push(Shot {
            ms: self.ms(),
            frame,
            caret: (caret.row, caret.col),
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

    fn replay(events: Vec<(u64, Rec)>) -> Self {
        let mut h = Self::new();
        for (ms, ev) in events {
            h.run_to(ms);
            match ev {
                Rec::In(b) => match b.as_slice() {
                    [0x7f] => h.owner.note_console_input(h.now, PetInputKind::Delete),
                    _ => h.owner.note_console_input(h.now, PetInputKind::Text),
                },
                Rec::Out(b) => {
                    let at = ClockReading {
                        monotonic: h.now,
                        wall_ms: None,
                    };
                    h.term.process_at(&b, at);
                }
            }
        }
        let end = h.ms() + 1_500;
        h.run_to(end);
        h
    }
}

/// A key: a character typed, or a delete.
#[derive(Clone, Copy)]
enum Key {
    Type(u8),
    Delete,
}

fn line_editor(row0: u16, keys: &[(u64, Key)]) -> Vec<(u64, Rec)> {
    let mut ev = Vec::new();
    let mut t = 300u64;
    ev.push((
        t,
        Rec::Out(format!("\x1b[2J\x1b[{};1H$ ", row0 + 1).into_bytes()),
    ));
    let (cols, rows) = (i32::from(COLS), i32::from(ROWS));
    let mut start_row = i32::from(row0);
    let mut n: i32 = 0;
    for &(gap, key) in keys {
        t += gap;
        let at = |n: i32, start_row: i32| (start_row + (PROMPT + n) / cols, (PROMPT + n) % cols);
        match key {
            Key::Type(ch) => {
                ev.push((t, Rec::In(vec![ch])));
                let (r, c) = at(n, start_row);
                let mut out = format!("\x1b[{};{}H{}", r + 1, c + 1, ch as char);
                n += 1;
                if c == cols - 1 {
                    out.push_str(" \r");
                    if r + 1 >= rows {
                        start_row -= 1;
                    }
                } else {
                    let (r, c) = at(n, start_row);
                    out.push_str(&format!("\x1b[{};{}H", r + 1, c + 1));
                }
                ev.push((t + 2, Rec::Out(out.into_bytes())));
            }
            Key::Delete if n > 0 => {
                ev.push((t, Rec::In(vec![0x7f])));
                n -= 1;
                let (r, c) = at(n, start_row);
                ev.push((
                    t + 2,
                    Rec::Out(format!("\x1b[{};{}H\x1b[J", r + 1, c + 1).into_bytes()),
                ));
            }
            Key::Delete => {}
        }
    }
    ev
}

const TEXT: &[u8] =
    b"the quick brown fox jumps over the lazy dog and keeps on running far past the \
barn and over the hill and down to the river where it stops to drink then goes on";

fn typed(n: usize, gap: u64) -> Vec<(u64, Key)> {
    TEXT.iter()
        .cycle()
        .take(n)
        .map(|&c| (gap, Key::Type(c)))
        .collect()
}

fn by_hand(n: usize) -> Vec<(u64, Key)> {
    let mut s = 7u64;
    TEXT.iter()
        .cycle()
        .take(n)
        .map(|&c| {
            s = s
                .wrapping_mul(6_364_136_223_846_793_005)
                .wrapping_add(1_442_695_040_888_963_407);
            let r = (s >> 33) % 100;
            let pause = if c == b' ' && r.is_multiple_of(3) {
                250
            } else {
                0
            };
            (70 + r + pause, Key::Type(c))
        })
        .collect()
}

type Flight = (u64, (f32, f32), (u16, u16));

/// What the replay saw from `from` ms on.
#[derive(Debug)]
struct Seen {
    flights: Vec<Flight>,
    /// Presents drawing the walk or the run, and all presents.
    legs: usize,
    presents: usize,
}

fn seen(shots: &[Shot], from: u64) -> Seen {
    let shots: Vec<&Shot> = shots.iter().filter(|s| s.ms >= from).collect();
    let mut flights = Vec::new();
    for (i, w) in shots.windows(2).enumerate() {
        if w[1].frame.action == PetAction::Leap && w[0].frame.action != PetAction::Leap {
            let down = shots[i + 1..]
                .iter()
                .find(|s| s.frame.action != PetAction::Leap)
                .map_or(((f32::NAN, f32::NAN), (0, 0)), |s| {
                    ((s.frame.col, s.frame.row), s.caret)
                });
            flights.push((w[1].ms, down.0, down.1));
        }
    }
    Seen {
        flights,
        legs: shots
            .iter()
            .filter(|s| matches!(s.frame.action, PetAction::Walk | PetAction::Run))
            .count(),
        presents: shots.len(),
    }
}

fn held_delete(row0: u16, len: usize, dels: usize, gap: u64) -> Seen {
    held_delete_paced(row0, len, dels, [gap, gap])
}

fn held_delete_paced(row0: u16, len: usize, dels: usize, gaps: [u64; 2]) -> Seen {
    held_delete_from(row0, len, dels, gaps).0
}

/// [`held_delete_paced`], and whether the cat stood on the row over the caret's as the deletes
/// began: the line's top, where the writing's approach to the margin sent it (THE CAT AT THE
/// MARGIN RIDES THE LINE).
fn held_delete_from(row0: u16, len: usize, dels: usize, gaps: [u64; 2]) -> (Seen, bool) {
    let mut keys = typed(len, 60);
    keys.push((1_500, Key::Type(b' ')));
    let from = 300 + keys.iter().map(|k| k.0).sum::<u64>();
    keys.extend((0..dels).map(|i| (gaps[i % 2], Key::Delete)));
    let h = Host::replay(line_editor(row0, &keys));
    let on_top =
        h.shots.iter().find(|s| s.ms >= from).is_some_and(|s| {
            s.caret.0 > 0 && (s.frame.row - (f32::from(s.caret.0) - 1.0)).abs() < 0.5
        });
    // A cat on the line's top is followed on its paws from the moment it is down on the
    // caret's row.
    let down = if on_top {
        h.shots
            .iter()
            .find(|s| {
                s.ms >= from
                    && s.frame.action != PetAction::Leap
                    && (s.frame.row - f32::from(s.caret.0)).abs() < 0.5
            })
            .map_or(u64::MAX, |s| s.ms)
    } else {
        from
    };
    let legs = seen(&h.shots, down);
    let all = seen(&h.shots, from);
    (
        Seen {
            flights: all.flights,
            legs: legs.legs,
            presents: legs.presents,
        },
        on_top,
    )
}

/// A HELD DELETE ON ONE ROW IS FOLLOWED ON THE PAWS: no flight while the caret runs back along
/// its row, and the cat on its legs for most of it. A cat the writing's approach to the margin
/// had sent onto the line's top (THE CAT AT THE MARGIN RIDES THE LINE, looks9 round 2) comes
/// down once, onto the caret's row, as the deletes take the caret off the margin, and is on its
/// legs for most of what follows.
#[test]
fn a_held_delete_on_one_row_is_followed_on_the_paws() {
    for gaps in [[32u64, 32], [48, 48], [64, 64], [10, 56], [12, 42]] {
        let (s, on_top) = held_delete_from(12, 70, 60, gaps);
        let allowed = usize::from(on_top);
        assert!(
            s.flights.len() <= allowed
                && s.flights
                    .iter()
                    .all(|&(_, (_, row), caret)| (row - f32::from(caret.0)).abs() < 0.5),
            "deletes {gaps:?} ms apart (on the line's top: {on_top}): flights {:?}",
            s.flights
        );
        assert!(
            s.presents >= 60,
            "deletes {gaps:?} ms apart: fixture: {} presents on the caret's row",
            s.presents
        );
        assert!(
            s.legs * 20 >= s.presents * 9,
            "deletes {gaps:?} ms apart: on its legs for {} of {} presents",
            s.legs,
            s.presents
        );
    }
}

#[test]
fn a_held_delete_across_a_wrap_is_one_bound_onto_the_carets_row() {
    for (row0, len, dels, wraps) in [
        (12u16, 130usize, 110usize, 1usize),
        (12, 220, 200, 2),
        (23, 130, 110, 1),
    ] {
        for gap in [32u64, 48, 64] {
            let s = held_delete(row0, len, dels, gap);
            let label = format!("{wraps} wrap(s) from row {row0}, a delete every {gap} ms");
            assert!(
                s.flights.len() <= wraps,
                "{label}: {} flights {:?}",
                s.flights.len(),
                s.flights
            );
            for &(ms, (_, row), caret) in &s.flights {
                assert!(
                    (row - f32::from(caret.0)).abs() < 0.5,
                    "{label}: the flight at {ms} ms came down on row {row}, the caret on {caret:?}"
                );
            }
        }
    }
}

/// Whether a flight that came down on `row` with the caret at `caret` is the margin's climb onto
/// the line ([`aterm_effects::kitty_pet::OldPath::WallUnderTheWriting`]): down on the row over
/// the caret's, before the wrap took the caret off that line.
fn is_climb(row: f32, caret: (u16, u16)) -> bool {
    caret.0 > 0 && (row - (f32::from(caret.0) - 1.0)).abs() < 0.5
}

/// A HELD KEY RUNS, AND EACH WRAP TYPED THROUGH IS ONE FLIGHT, down on the caret's new row. The
/// line's first row, with a clear row over it, may cost one flight more, before its wrap: the
/// cat's climb onto the line as the writing comes to the margin (THE CAT AT THE MARGIN RIDES THE
/// LINE, looks9 round 2), which comes down on the row over the caret's while the caret is still
/// on it. A wrap whose row has letters over it (the wrapped line's later rows) costs only its
/// crossing; so does writing too fast to land the climb before its wrap (33 keys a second). Each
/// crossing is in the air no longer than the brisk bound's longest shape (850 ms).
#[test]
fn a_held_key_runs_and_each_wrap_typed_through_is_one_flight() {
    let takes: [(&str, Vec<(u64, Key)>); 3] = [
        ("33 keys a second", typed(240, 30)),
        ("20 keys a second", typed(240, 50)),
        ("by hand", by_hand(240)),
    ];
    for (name, keys) in &takes {
        for row0 in [12u16, 23] {
            let h = Host::replay(line_editor(row0, keys));
            let from = h
                .shots
                .iter()
                .find(|s| s.caret.1 >= 12 && s.frame.action != PetAction::Leap)
                .map_or(0, |s| s.ms);
            let s = seen(&h.shots, from);
            let aloft = airtimes(&h.shots, from);
            assert!(
                !aloft.is_empty() && aloft.iter().all(|&(_, a)| a <= 850),
                "{name}, the prompt on row {row0}: crossings (ms, ms aloft) {aloft:?}"
            );
            let label = format!("{name}, the prompt on row {row0}");
            let climbs: Vec<_> = s
                .flights
                .iter()
                .filter(|&&(_, (_, row), caret)| is_climb(row, caret))
                .collect();
            assert_eq!(
                s.flights.len() - climbs.len(),
                3,
                "{label}: three wraps, flights {:?}",
                s.flights
            );
            assert!(
                climbs.len() <= 1 && climbs.iter().all(|c| c.0 < s.flights[climbs.len()].0),
                "{label}: more than the one climb onto the line before its first wrap, flights \
                 {:?}",
                s.flights
            );
            for &(ms, (_, row), caret) in &s.flights {
                assert!(
                    (row - f32::from(caret.0)).abs() < 0.5 || is_climb(row, caret),
                    "{label}: the flight at {ms} ms came down on row {row}, the caret on {caret:?}"
                );
            }
        }
    }
}

/// How long each flight from `from` ms on is in the air: `(the present it leaves on, ms aloft)`.
fn airtimes(shots: &[Shot], from: u64) -> Vec<(u64, u64)> {
    let shots: Vec<&Shot> = shots.iter().filter(|s| s.ms >= from).collect();
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

/// A take of the wrap law: `(label, the prompt's row, the keys, ms the judged span starts at)`.
type WrapTake = (String, u16, Vec<(u64, Key)>, u64);

/// THE WRITING'S WRAP IS A BRISK BOUND, AT THE SHELL TOO. A line typed past the margin wraps
/// onto the next line (mid-screen) or onto the line the screen scrolls up (the prompt on the
/// last row), and the cat crosses after the wrap; a held delete takes the caret back across a
/// wrap, and the cat crosses after it. Each crossing is the animal's own pace on its arc, in the
/// air no longer than the big bound's longest shape (850 ms). Judged here: a held delete taking
/// the caret back across one and two wraps, from mid-screen and from the last row. The crossings
/// typed at 33 and 20 keys a second and by hand are judged by
/// [`a_held_key_runs_and_each_wrap_typed_through_is_one_flight`], and those typed on after a
/// pause at the margin by [`a_pause_at_the_margin_never_keeps_the_cat_in_the_air`] (with
/// [`typing_on_after_a_pause_at_the_margin_costs_only_the_wraps_flight`]).
///
/// NEGATIVE CONTROL (as run on main before the fix): every crossing 0.99 to 1.17 s in the air,
/// paced as a quiet crossing, 0.74 of the pane a second. With the wrap the writing's only while
/// its rhythm was open and the cat on the ground, a wrap typed two or three keys after a pause
/// crossed in 1.2 s after a perk, and one that came as a hop came down in 1.17 s. Mid-screen,
/// with the leap ahead of a wrap that was coming (3984c54ccb), each wrap's flight left on a
/// guess of when the wrap would come and hung over the line for it: 1.04 to 1.55 s in the air.
#[test]
fn each_wrap_typed_through_is_a_brisk_bound() {
    let mut takes: Vec<WrapTake> = Vec::new();
    for row0 in [12u16, 23] {
        for (len, dels) in [(130usize, 110usize), (220, 200)] {
            let mut keys = typed(len, 60);
            keys.push((1_500, Key::Type(b' ')));
            let from = 300 + keys.iter().map(|k| k.0).sum::<u64>();
            keys.extend((0..dels).map(|_| (48, Key::Delete)));
            takes.push((
                format!("{dels} deletes back from {len} keys, the prompt on row {row0}"),
                row0,
                keys,
                from,
            ));
        }
    }
    for (label, row0, keys, from) in &takes {
        let h = Host::replay(line_editor(*row0, keys));
        let flights = airtimes(&h.shots, *from);
        assert!(
            !flights.is_empty(),
            "{label}: fixture — the wraps are flown"
        );
        assert!(
            flights.iter().all(|&(_, aloft)| aloft <= 850),
            "{label}: crossings (ms, ms aloft) {flights:?}"
        );
    }
}

fn faintest_in_flight(shots: &[Shot]) -> (f32, u64) {
    let mut worst = (1.0f32, 0u64);
    let mut i = 1;
    while i < shots.len() {
        let (prev, s) = (&shots[i - 1], &shots[i]);
        if s.frame.action == PetAction::Leap
            && prev.frame.action != PetAction::Leap
            && prev.frame.alpha > 0
        {
            let launch = f32::from(prev.frame.alpha);
            let landed = shots[i..]
                .iter()
                .position(|s| s.frame.action != PetAction::Leap)
                .map_or(shots.len(), |k| i + k);
            let until = shots.get(landed).map_or(u64::MAX, |s| s.ms + 500);
            for s in shots[i..].iter().take_while(|s| s.ms <= until) {
                let share = f32::from(s.frame.alpha) / launch;
                if share < worst.0 {
                    worst = (share, s.ms);
                }
            }
            i = landed;
        }
        i += 1;
    }
    worst
}

fn brightest_in_flight(shots: &[Shot]) -> (f32, u64) {
    let mut worst = (0.0f32, 0u64);
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
        let gathered = f32::from(shots[g - 1].frame.alpha);
        let mut end = i;
        while end + 1 < shots.len()
            && matches!(
                shots[end + 1].frame.action,
                PetAction::Leap | PetAction::Land
            )
        {
            end += 1;
        }
        if gathered > 0.0 {
            for s in &shots[g..=end] {
                let share = f32::from(s.frame.alpha) / gathered;
                if share > worst.0 {
                    worst = (share, s.ms);
                }
            }
        }
        i = end + 1;
    }
    worst
}

#[test]
fn a_flight_over_the_words_keeps_its_body() {
    let mut keys = typed(130, 60);
    keys.push((1_500, Key::Type(b' ')));
    keys.extend((0..110).map(|_| (32, Key::Delete)));
    let takes: [(&str, Vec<(u64, Rec)>); 2] = [
        ("a held delete across a wrap", line_editor(12, &keys)),
        (
            "typed through the bottom row",
            line_editor(23, &by_hand(240)),
        ),
    ];
    for (name, events) in takes {
        let h = Host::replay(events);
        let flights = seen(&h.shots, 0).flights.len();
        assert!(flights >= 2, "{name}: the take flies ({flights})");
        let (share, at) = faintest_in_flight(&h.shots);
        assert!(
            share >= 0.9,
            "{name}: at {at} ms the cat drawn at {:.0}% of the presence it left with",
            share * 100.0
        );
        let (share, at) = brightest_in_flight(&h.shots);
        assert!(
            share <= 1.0,
            "{name}: at {at} ms the cat drawn at {:.0}% of the presence it gathered with",
            share * 100.0
        );
    }
}

fn flashes(shots: &[Shot]) -> Vec<(u64, String, String, String, u64)> {
    const POSE_MS: u64 = 25;
    let family = |s: &Shot| {
        let id = aterm_effects::pet_glyphs_gen::PET_GLYPHS[s.frame.pose as usize].id;
        match id
            .trim_end_matches(|c: char| c.is_ascii_digit())
            .trim_end_matches('_')
        {
            "pet_walk" | "pet_lwalk" | "pet_run" => "leg".to_string(),
            "pet_launch" | "pet_leap_rise" | "pet_leap" | "pet_hop" | "pet_apex"
            | "pet_leap_descend" => "air".to_string(),
            other => other.to_string(),
        }
    };
    let lit: Vec<&Shot> = shots.iter().filter(|s| s.frame.alpha > 0).collect();
    let fams: Vec<String> = lit.iter().map(|s| family(s)).collect();
    let mut out = Vec::new();
    let mut i = 1;
    while i < lit.len() {
        if fams[i] == fams[i - 1] {
            i += 1;
            continue;
        }
        let mut j = i;
        while j + 1 < lit.len() && fams[j + 1] == fams[i] {
            j += 1;
        }
        if j + 1 < lit.len() && lit[j + 1].ms - lit[i].ms < POSE_MS {
            out.push((
                lit[i].ms,
                fams[i - 1].clone(),
                fams[i].clone(),
                fams[j + 1].clone(),
                lit[j + 1].ms - lit[i].ms,
            ));
        }
        i = j + 1;
    }
    out
}

type NamedTake = (String, u16, Vec<(u64, Key)>);

#[test]
fn no_pose_flashes_as_a_line_is_typed_and_held() {
    let mut takes: Vec<NamedTake> = Vec::new();
    for gap in [66u64, 50, 30] {
        for row0 in [12u16, 23] {
            takes.push((
                format!("typed {gap} ms a key, row {row0}"),
                row0,
                typed(220, gap),
            ));
            let mut keys = typed(130, 60);
            keys.push((1_500, Key::Type(b' ')));
            keys.extend((0..110).map(|_| (gap.min(48), Key::Delete)));
            takes.push((format!("a held delete after it, row {row0}"), row0, keys));
        }
    }
    let seen = crate::common::par_map(&takes, |(name, row0, keys)| {
        let h = Host::replay(line_editor(*row0, keys));
        (name.clone(), flashes(&h.shots))
    });
    for (name, flashes) in &seen {
        assert!(
            flashes.is_empty(),
            "{name}: poses on glass under two frames (ms, before, pose, after, ms): {:?}",
            &flashes[..flashes.len().min(6)]
        );
    }
}

/// A line typed toward the margin and a pause: `n0` keys `gap0` ms apart, then `n1` more `gap1`
/// ms apart, the first of them `pause` ms after the last of the `n0` (none where `n1` is 0).
fn paused(n0: usize, gap0: u64, pause: u64, n1: usize, gap1: u64) -> Vec<(u64, Key)> {
    let mut keys = typed(n0, gap0);
    if n1 > 0 {
        keys.extend(typed(n0 + n1, gap1).into_iter().skip(n0));
        keys[n0].0 = pause;
    }
    keys
}

/// The replay of `keys` typed from a prompt on `row0`, watched for `tail` ms past the last key.
fn replay_watched(row0: u16, keys: &[(u64, Key)], tail: u64) -> Host {
    let mut ev = line_editor(row0, keys);
    let last = ev.last().map_or(0, |e| e.0);
    ev.push((last + tail, Rec::Out(Vec::new())));
    Host::replay(ev)
}

/// When each key of `keys` is pressed (ms), as [`line_editor`] presses them.
fn key_times(keys: &[(u64, Key)]) -> Vec<u64> {
    keys.iter()
        .scan(300u64, |t, k| {
            *t += k.0;
            Some(*t)
        })
        .collect()
}

/// A flight: `(ms it leaves, its landing (col, row) or NaN where it is still in the air at the
/// end, ms aloft, the caret at its landing, the caret as it left)`.
type Aloft = (u64, (f32, f32), u64, (u16, u16), (u16, u16));

/// Each flight from `from` ms on.
fn flights_from(shots: &[Shot], from: u64) -> Vec<Aloft> {
    let shots: Vec<&Shot> = shots.iter().filter(|s| s.ms >= from).collect();
    let end = shots.last().map_or(0, |s| s.ms);
    shots
        .windows(2)
        .enumerate()
        .filter(|(_, w)| {
            w[1].frame.action == PetAction::Leap && w[0].frame.action != PetAction::Leap
        })
        .map(|(i, w)| {
            match shots[i + 1..]
                .iter()
                .find(|s| s.frame.action != PetAction::Leap)
            {
                Some(d) => (
                    w[1].ms,
                    (d.frame.col, d.frame.row),
                    d.ms - w[1].ms,
                    d.caret,
                    w[1].caret,
                ),
                None => (
                    w[1].ms,
                    (f32::NAN, f32::NAN),
                    end - w[1].ms,
                    w[1].caret,
                    w[1].caret,
                ),
            }
        })
        .collect()
}

/// The pauses at the margin: `(n0, gap0, pause, n1, gap1)` as [`paused`] types them. The prompt
/// takes two cells, so the caret pauses at `n0 + 2` of 80: on the cells the cat stands on at the
/// wall (77, 78, 79) or short of them (72, 74), for less and more than the cat's sit, by hand
/// and held, typed on through the wrap after it or never; typed on just after the cat's sit
/// (its hop off the letters in the air as the line wraps); and stopped at the margin of the
/// line's second row (`n0` past 78: the caret at `n0 + 2 - 80`, the line's first row over it).
const MARGIN_PAUSES: [(usize, u64, u64, usize, u64); 19] = [
    (75, 150, 800, 9, 130),
    (75, 100, 1_500, 9, 120),
    (70, 100, 1_500, 14, 120),
    (76, 90, 2_500, 8, 150),
    (72, 120, 1_000, 10, 120),
    (74, 60, 900, 10, 60),
    (77, 100, 1_800, 6, 100),
    (72, 100, 4_000, 14, 120),
    (77, 33, 3_000, 8, 33),
    (76, 100, 0, 0, 0),
    (72, 66, 0, 0, 0),
    (75, 33, 0, 0, 0),
    (77, 33, 0, 0, 0),
    (75, 90, 1_450, 9, 100),
    (76, 90, 1_500, 8, 160),
    (77, 90, 1_600, 7, 100),
    (152, 70, 0, 0, 0),
    (155, 70, 0, 0, 0),
    (157, 70, 0, 0, 0),
];

/// How far (cells) from the caret, along the row, a cat waiting by it may stand.
const BY_THE_CARET: f32 = 8.0;

/// A PAUSE AT THE MARGIN NEVER KEEPS THE CAT IN THE AIR. Typed toward the right margin and
/// paused there, on the top row, mid-screen and on the last row, the cat waits on the ground by
/// the caret: no flight is in the air longer than the brisk bound's longest (850 ms); one that
/// leaves while the hand is paused is a hop of under 0.3 s that comes down by the caret it left
/// (within two rows and [`BY_THE_CARET`] cells), even where the hand types on through the wrap
/// while it is in the air; and typing that stops short of the wrap leaves the cat on the ground
/// by the caret. Every take of [`MARGIN_PAUSES`].
///
/// NEGATIVE CONTROL (as run on main before the fix, 3984c54ccb): mid-screen the pause launched
/// the leap ahead of the wrap, which hung for it: 1.34 to 1.81 s in the air, coming down on the
/// next line's start, 70 cells from a caret that had not wrapped (and, for typing that stopped
/// there, never did). With the hop off the letters but a wrap that came while it was in the air
/// drawing it out to the next line's start at a hop's pace (75d1af3f53): 1.30 to 1.55 s in the
/// air, gliding low over the line it left.
#[test]
fn a_pause_at_the_margin_never_keeps_the_cat_in_the_air() {
    let mut takes = Vec::new();
    for row0 in [0u16, 12, 23] {
        for take in MARGIN_PAUSES {
            takes.push((row0, take));
        }
    }
    let seen = crate::common::par_map(&takes, |&(row0, (n0, gap0, pause, n1, gap1))| {
        let keys = paused(n0, gap0, pause, n1, gap1);
        let at = key_times(&keys);
        let h = replay_watched(row0, &keys, 6_000);
        let from = h
            .shots
            .iter()
            .find(|s| s.caret.1 >= 12 && s.frame.action != PetAction::Leap)
            .map_or(0, |s| s.ms);
        let (paused_from, paused_to) = (at[n0 - 1], at.get(n0).copied().unwrap_or(u64::MAX));
        let label = format!("{n0} keys {gap0} ms apart, {pause} ms, {n1} keys, row {row0}");
        let mut faults = Vec::new();
        for (ms, (col, row), aloft, _, left) in flights_from(&h.shots, from) {
            if aloft > 850 {
                faults.push(format!(
                    "{label}: the flight at {ms} ms was {aloft} ms in the air"
                ));
            }
            let near = (row - f32::from(left.0)).abs() <= 2.0
                && (col - f32::from(left.1)).abs() <= BY_THE_CARET;
            if ms > paused_from && ms < paused_to && (aloft >= 300 || !near) {
                faults.push(format!(
                    "{label}: the hand paused, yet the flight at {ms} ms ({aloft} ms) came down \
                     at ({col:.1}, {row:.1}), the caret at {left:?} as it left"
                ));
            }
        }
        if n1 == 0 {
            let last = h.shots.last().expect("presents");
            let (cr, cc) = last.caret;
            if last.frame.action == PetAction::Leap
                || (last.frame.row - f32::from(cr)).abs() > 2.0
                || (last.frame.col - f32::from(cc)).abs() > BY_THE_CARET
            {
                faults.push(format!(
                    "{label}: the typing stopped short of the wrap, yet the cat ended {:?} at \
                     ({:.1}, {:.1}), the caret at {:?}",
                    last.frame.action, last.frame.col, last.frame.row, last.caret
                ));
            }
        }
        faults
    });
    let faults: Vec<String> = seen.into_iter().flatten().collect();
    assert!(faults.is_empty(), "{}", faults.join("\n"));
}

/// TYPING ON AFTER A PAUSE AT THE MARGIN COSTS ONLY THE WRAP'S FLIGHT. Once the hand types on
/// after a pause at the margin, on the top row, mid-screen and on the last row, the line end
/// costs the cat one flight, the wrap's crossing, down on the caret's row, and at most the climb
/// onto the line before it ([`is_climb`]) as the writing comes to the margin, the line's only
/// climb, whether it came before the pause or in it (the writing's pace brought the caret within
/// the climb's lead of the wall seat as the hand stopped). Every take of [`MARGIN_PAUSES`] that
/// types on.
///
/// NEGATIVE CONTROL (as run before the fix): the keys typed after the pause, short of the
/// writing's rhythm, found the margin untaken: the cat hopped up a row (the wall side crossed to
/// the caret's left, over the line's words), on the last row hopped back to the wall as the
/// rhythm opened, and crossed at the wrap: three flights in a second there, two on the top row
/// and mid-screen.
#[test]
fn typing_on_after_a_pause_at_the_margin_costs_only_the_wraps_flight() {
    let mut takes = Vec::new();
    for row0 in [0u16, 12, 23] {
        for take in MARGIN_PAUSES.into_iter().filter(|t| t.3 > 0) {
            takes.push((row0, take));
        }
    }
    let seen = crate::common::par_map(&takes, |&(row0, (n0, gap0, pause, n1, gap1))| {
        let keys = paused(n0, gap0, pause, n1, gap1);
        let at = key_times(&keys);
        let h = replay_watched(row0, &keys, 3_000);
        let label = format!("{n0} keys {gap0} ms apart, {pause} ms, {n1} keys, row {row0}");
        let mut faults = Vec::new();
        let resumed = flights_from(&h.shots, at[n0]);
        let crossings = |flights: &[Aloft]| {
            let climbs = flights
                .iter()
                .filter(|&&(_, (_, row), _, caret, _)| is_climb(row, caret))
                .count();
            (flights.len() - climbs, climbs)
        };
        let (crossed, climbed) = crossings(&resumed);
        if crossed != 1 || climbed > 1 {
            faults.push(format!(
                "{label}: {} flights after the typing resumed (ms, landing, ms aloft, caret): \
                 {resumed:?}",
                resumed.len()
            ));
        }
        for &(ms, (_, row), _, caret, _) in &resumed {
            if (row - f32::from(caret.0)).abs() >= 0.5 && !is_climb(row, caret) {
                faults.push(format!(
                    "{label}: the flight at {ms} ms came down on row {row}, the caret on {caret:?}"
                ));
            }
        }
        // The prompt takes two cells; the wall seat (74.77 of 80) covers no letter while the
        // caret stands at 75 or short of it, so the pause costs no flight but the climb.
        if n0 + 2 <= 75 {
            let line_end = flights_from(&h.shots, at[n0 - 1]);
            let (crossed, climbed) = crossings(&line_end);
            if crossed != 1 || climbed > 1 {
                faults.push(format!(
                    "{label}: {} flights from the pause on: {line_end:?}",
                    line_end.len()
                ));
            }
        }
        faults
    });
    let faults: Vec<String> = seen.into_iter().flatten().collect();
    assert!(faults.is_empty(), "{}", faults.join("\n"));
}

/// The first cell with a glyph on it that the body's silhouette (its drawn box, shaved of the
/// tile's transparent border at the sides) meets, by any part of a pixel row.
fn body_meets_glyphs(input: &RenderInput, frame: &PetFrame) -> Option<(u16, u16)> {
    const CELL_W: u16 = 15;
    const CELL_H: u16 = 28;
    const TILE_BORDER: f32 = 0.30;
    let (x0, x1, y0, y1) = frame.body_px(CELL_W, CELL_H, COLS, ROWS)?;
    let border = (TILE_BORDER * f32::from(CELL_W)).round() as i32;
    let (x0, x1) = (x0 + border, x1 - border);
    let (cw, ch) = (i32::from(CELL_W), i32::from(CELL_H));
    for row in y0.div_euclid(ch).max(0)..=((y1 - 1).div_euclid(ch)).min(i32::from(ROWS) - 1) {
        for col in x0.div_euclid(cw).max(0)..=((x1 - 1).div_euclid(cw)).min(i32::from(COLS) - 1) {
            if input.cells[row as usize]
                .get(col as usize)
                .is_some_and(|c| c.ch != ' ' && c.ch != '\0')
            {
                return Some((row as u16, col as u16));
            }
        }
    }
    None
}

/// A CAT THE HAND LEAVES AT THE MARGIN RESTS OFF ITS LETTERS, BY THE CARET. Typing that stops
/// at the margin with the line's last letters over the cat's stand at the wall, or at the margin
/// of a wrapped line's second row under the first row's letters, on the top row, mid-screen and
/// on the last row, by hand or held: once the hand has stopped (the cat sits, 1.4 s after the
/// last key, and its hop has landed) every frame of its rest stands off every glyph on the
/// screen, within two rows and [`BY_THE_CARET`] cells of the caret.
///
/// NEGATIVE CONTROL (as run on main before the fix, 3984c54ccb): on the last row the cat sat on
/// the line's last letters for good; mid-screen it leapt ahead to the next line's start for a
/// wrap that never came, 70 cells from the caret. On a wrapped line's second row (75d1af3f53),
/// mid-screen the seat two rows under the caret was let go once the cat had landed on it, and it
/// hopped again onto a seat with its head among the caret's line; on the last row it sat at the
/// wall with its head among the first row's letters.
#[test]
fn a_cat_the_hand_leaves_at_the_margin_rests_off_its_letters() {
    let mut takes = Vec::new();
    for row0 in [0u16, 12, 23] {
        for take in MARGIN_PAUSES
            .into_iter()
            .filter(|t| t.3 == 0 && t.0 + 2 >= 77)
        {
            takes.push((row0, take));
        }
    }
    let seen = crate::common::par_map(&takes, |&(row0, (n0, gap0, pause, n1, gap1))| {
        let keys = paused(n0, gap0, pause, n1, gap1);
        let at = key_times(&keys);
        let mut h = replay_watched(row0, &keys, 6_000);
        let input = h.term.cell_frame(usize::from(ROWS), usize::from(COLS));
        let label = format!("{n0} keys {gap0} ms apart, stopped, row {row0}");
        let rest_from = at[n0 - 1] + 2_000;
        let rests: Vec<&Shot> = h
            .shots
            .iter()
            .filter(|s| s.ms >= rest_from && s.frame.alpha > 0)
            .collect();
        let mut faults = Vec::new();
        if rests.len() < 200 {
            faults.push(format!("{label}: fixture: {} frames of rest", rests.len()));
        }
        for s in rests {
            let (cr, cc) = s.caret;
            let met = body_meets_glyphs(&input, &s.frame);
            let near = (s.frame.row - f32::from(cr)).abs() <= 2.0
                && (s.frame.col - f32::from(cc)).abs() <= BY_THE_CARET;
            if !s.frame.action.settled() || met.is_some() || !near {
                faults.push(format!(
                    "{label}: {} ms after the last key, {:?} at ({:.2}, {:.2}), the caret at \
                     {:?}, meets a glyph at {met:?}",
                    s.ms - at[n0 - 1],
                    s.frame.action,
                    s.frame.col,
                    s.frame.row,
                    s.caret
                ));
                break;
            }
        }
        faults
    });
    let faults: Vec<String> = seen.into_iter().flatten().collect();
    assert!(faults.is_empty(), "{}", faults.join("\n"));
}
