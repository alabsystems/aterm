// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! The cat follows a running caret on its paws, both ways, and crosses a soft wrap in one
//! flight.
//!
//! A held key repeats 15 to 33 times a second. The cat ran no faster than 26 cells a second,
//! its startle planted its feet for 0.38 s, a run back fed it none of the caret's pace, and a
//! gap of 14 cells opened the pounce: a held backspace was followed in a chain of bounds that
//! landed short, sat out their landing and bounded again, and across a soft wrap one bound
//! flew the width of the row the caret had just left before a hop up to the caret's row
//! (2026-10-21, the owner's "jumping": three leaps for one held key, one of them away from the
//! caret). Typed on into the right margin, the cat hopped back over the caret onto the words
//! just typed and then bounded the line at the wrap: two flights at every line end.
//!
//! These laws replay a line editor's bytes (the caret placed after every key, its wrap the
//! terminal's own autowrap) through a real `Terminal` and the shipping `CompanionOwner`,
//! presented every 16 ms, with each key noted as the app notes it.

use std::time::{Duration, Instant};

use aterm_core::render::RenderInput;
use aterm_core::terminal::{ClockReading, Terminal};
use aterm_effects::companion::{CompanionOwner, GlowOwnership, PetFacts};
use aterm_effects::cursor_glow::GlowStyle;
use aterm_effects::host::{
    CaptureMode, FrameGeom, HostFrameInput, SingFacts, TerminalFacts, Visibility,
};
use aterm_effects::kitty_pet::{PetAction, PetFrame, PetInputKind};
use aterm_effects::pet_world::{PetPane, PetReserve, PetWorldFacts};
use aterm_effects::word_decorations::{DecoConfig, WordDecorations};
use aterm_lexicon::Lexicon;

use crate::common::{FrameHold, Rec};

const ROWS: u16 = 24;
const COLS: u16 = 80;
const FRAME_MS: u64 = 16;
/// The prompt `$ `.
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

    /// The native order: observe, prepare, tick, rescan.
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
                rows: ROWS,
                cols: COLS,
                cell_w: 15,
                cell_h: 28,
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

/// A line editor's take: the prompt `$ ` at `row0`, then `keys`, each `(ms after the last,
/// key)`, echoed 2 ms later. A character is written where the caret is and the caret placed
/// after it; at the right margin a space and a CR take the terminal's own autowrap (which
/// scrolls at the bottom row), as zsh does. A delete places the caret back and clears the
/// rest of the screen. Returns the events and when the keys began.
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

/// `n` characters at `gap` ms a key.
fn typed(n: usize, gap: u64) -> Vec<(u64, Key)> {
    TEXT.iter()
        .cycle()
        .take(n)
        .map(|&c| (gap, Key::Type(c)))
        .collect()
}

/// `n` characters at a hand's uneven pace: 70 to 170 ms a key, a third of the spaces held a
/// quarter second longer (a fixed seed).
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

/// A flight: `(ms it left, where it came down (col, row), the caret then)`.
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

/// A line typed (`len` keys at 60 ms), a pause, then `dels` deletes `gap` ms apart, from a
/// prompt at `row0`; what the replay saw from the first delete on.
fn held_delete(row0: u16, len: usize, dels: usize, gap: u64) -> Seen {
    held_delete_paced(row0, len, dels, [gap, gap])
}

/// [`held_delete`] with the deletes' gaps alternating `gaps[0]`, `gaps[1]` ms: a held key's
/// repeat as the terminal delivers it, where a pair of echoes can land in one frame.
fn held_delete_paced(row0: u16, len: usize, dels: usize, gaps: [u64; 2]) -> Seen {
    let mut keys = typed(len, 60);
    keys.push((1_500, Key::Type(b' ')));
    let from = 300 + keys.iter().map(|k| k.0).sum::<u64>();
    keys.extend((0..dels).map(|i| (gaps[i % 2], Key::Delete)));
    let h = Host::replay(line_editor(row0, &keys));
    seen(&h.shots, from)
}

/// A HELD DELETE ON ONE ROW IS FOLLOWED ON THE PAWS: at 31, 21 and 16 deletes a second, evenly
/// and as a terminal delivers a held key (gaps of 10 and 56 ms, or 12 and 42, a pair of echoes
/// in one frame), no flight, and the cat walks or runs through nearly half the hold or more
/// (the rest is its startle and its stop). Negative
/// controls: the cat as it ran on 2026-10-21 (no faster than 26 cells a second, feet planted
/// for the whole startle, no pace fed back): two bounds at 31 a second, the cat airborne or
/// coiled for 136 of 214 presents and on its legs for 17; and the run's pace taken step by
/// step (a pair of echoes in one frame read as a hundred cells a second, past the gallop),
/// with the pounce kept for a gap the gallop would close in over 1.2 s: the even holds ran,
/// the uneven ones pounced twice, on their legs for 0 to 31 of about 200 presents (as a live
/// instance did under the control socket's own key timing).
#[test]
fn a_held_delete_on_one_row_is_followed_on_the_paws() {
    for gaps in [[32u64, 32], [48, 48], [64, 64], [10, 56], [12, 42]] {
        let s = held_delete_paced(12, 70, 60, gaps);
        assert!(
            s.flights.is_empty(),
            "deletes {gaps:?} ms apart: flights {:?}",
            s.flights
        );
        assert!(
            s.legs * 20 >= s.presents * 9,
            "deletes {gaps:?} ms apart: on its legs for {} of {} presents",
            s.legs,
            s.presents
        );
    }
}

/// A HELD DELETE ACROSS A SOFT WRAP IS ONE BOUND A WRAP, ONTO THE CARET'S ROW: across one wrap
/// and two, mid-screen and at the bottom row, at 31, 21 and 16 deletes a second, no more
/// flights than wraps, and every flight comes down on the row the caret is on. Negative
/// control (as run on 2026-10-21): three or four flights for one wrap, one coming down on
/// the row the caret had left, a screen's width from it.
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

/// A HELD KEY IS FOLLOWED ON THE PAWS, AND EVERY WRAP TYPED THROUGH IS ONE FLIGHT: typed at a
/// held key's 33 and 20 a second and by hand, mid-screen and on the bottom row (whose wrap
/// scrolls), the line's three wraps take one flight each, every one coming down on the row
/// the caret is on, and nothing else flies. Negative controls (as run on 2026-10-21): the
/// hop over the caret at the margin and the bound at the wrap, two flights a wrap; at 33 keys
/// a second also a chain of pounces along each line.
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
            // From the cat's first walk along the line (its first sighting's trip is not
            // this law's).
            let from = h
                .shots
                .iter()
                .find(|s| s.caret.1 >= 12 && s.frame.action != PetAction::Leap)
                .map_or(0, |s| s.ms);
            let s = seen(&h.shots, from);
            let label = format!("{name}, the prompt on row {row0}");
            assert_eq!(
                s.flights.len(),
                3,
                "{label}: three wraps, flights {:?}",
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

/// The faintest present, against the present each flight left from, from every launch of a
/// body on glass to half a second after its landing: `(that share, ms)`.
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

/// A FLIGHT OVER THE WORDS KEEPS ITS BODY. The bound back across a held delete's wrap flies
/// over the line it is deleting, and a wrap typed through at the bottom row bounds beside the
/// line just scrolled up. Veiled by each frame's box, the cat went from full to a fifth of
/// itself and back within one flight, three times in two seconds at a wrap, and held-delete
/// pounces flashed solid at every landing (2026-10-21, both themes: the owner's "blinking").
/// From every launch to half a second after its landing, no present draws it below nine
/// tenths of the presence it left with. Negative control (as run on 2026-10-21): 56 of 255.
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
    }
}

/// Each pose that went on glass and came off again in under [`POSE_MS`] between two other
/// poses, as `(ms it began, before, pose, after, ms on glass)`, by the art's family: a leg's
/// cycle (the walk's, the amble's, the run's) is one family, as are the flight's frames.
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

/// A named line-editor take: its name, the prompt's row and its keys ([`line_editor`]).
type NamedTake = (String, u16, Vec<(u64, Key)>);

/// NO POSE FLASHES AS A LINE IS TYPED AND HELD: typed at 15, 20 and 33 keys a second through
/// two wraps, mid-screen and on the bottom row, and a held delete back across them, every pose
/// the cat takes stays on glass two 60 Hz frames before the next. Negative control (as run on
/// 2026-10-22): typed at 15 a second toward the margin, the cat stopped in its stand, and the
/// wrap's coil, timed under the stand's held stride, came out from behind it on its last
/// frame: one frame of the crouch between the stand and the launch, at every wrap.
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
