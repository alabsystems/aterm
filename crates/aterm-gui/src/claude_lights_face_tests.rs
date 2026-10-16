// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! THE FACE TABLE (`docs/DESIGN-harness-autopilot-2026-10-15.md` §9.6.0):
//! every face the Autopilot chip wears for every state of its glass
//! (§§9.2–9.5), its inks, its whole and short titles and its spoken label;
//! its glyph-only rung; the default glass giving v1's chip exactly; and the
//! repaint term's bit table.

use super::*;

use aterm_agent::harness::acts::{Act, TaskFace};
use aterm_agent::supervise::policy::turn_end::LAND_NUDGES;

/// The footer's mark accent, as a theme gives it: Cruise's ink.
const ACCENT: [u8; 3] = [0, 128, 255];

fn blank() -> RenderCell {
    RenderCell {
        ch: ' ',
        fg: [200, 200, 200],
        bg: [0, 0, 0],
        ..RenderCell::default()
    }
}

fn text(cells: &[RenderCell]) -> String {
    cells.iter().map(|c| c.ch).collect()
}

/// 18:00, 16:00 and 17:42 as the glass carries them.
fn clock(h: u16, m: u16) -> Clock {
    Clock::of_minute(h * 60 + m)
}

/// A timer landing at `h:m`, on another day where `day` names one.
fn timer(h: u16, m: u16, day: Option<Weekday>) -> ChipGlass {
    ChipGlass {
        land_at: Some(LandAtGlass {
            clock: clock(h, m),
            other_day: day,
        }),
        ..ChipGlass::default()
    }
}

/// The task's face told at 17:42.
fn told(face: TaskFace) -> ChipGlass {
    ChipGlass {
        task: Some(face),
        task_at: Some(clock(17, 42)),
        ..ChipGlass::default()
    }
}

/// One state of the chip: its view and its glass, and what the table gives
/// it — the face as drawn, its glyph-only rung, its inks, its whole title
/// (no trail, no menu), its short title and its spoken label.
struct Row {
    view: AutopilotView,
    glass: ChipGlass,
    face: ChipFace,
    drawn: &'static str,
    glyph: &'static str,
    tones: (ChipTone, ChipTone),
    whole: String,
    short: &'static str,
    spoken: String,
}

/// Every state of §§9.2–9.5, in the words of the design.
fn rows() -> Vec<Row> {
    use ChipTone::{Accent, Amber, Dim};
    let row = |view, glass, face, drawn, glyph, tones, whole: &str, short, spoken: &str| Row {
        view,
        glass,
        face,
        drawn,
        glyph,
        tones,
        whole: whole.to_owned(),
        short,
        spoken: spoken.to_owned(),
    };
    let spent = format!(
        "autopilot: land, quiet \u{2014} no done reply after the instruction and {LAND_NUDGES} \
         nudges; the harness types nothing more but a land answer to a question \u{b7} click: off"
    );
    let spent_spoken = format!("Autopilot: land. Quiet: no done reply after {LAND_NUDGES} nudges.");
    vec![
        row(
            AutopilotView::Cruise,
            ChipGlass::default(),
            ChipFace::Cruise,
            "\u{221e}",
            "\u{221e}",
            (Accent, Accent),
            "autopilot: cruise \u{2014} keeps the work moving \u{b7} click: land",
            "autopilot: cruise",
            "Autopilot: cruise.",
        ),
        row(
            AutopilotView::Cruise,
            timer(18, 0, None),
            ChipFace::CruiseUntil,
            "\u{221e} \u{2913}18:00",
            "\u{221e}",
            (Accent, Amber),
            "autopilot: cruise until 18:00, then land \u{2014} keeps the work moving \u{b7} \
             click: land now",
            "autopilot: cruise until 18:00, then land",
            "Autopilot: cruise. Lands at 18:00.",
        ),
        row(
            AutopilotView::Cruise,
            timer(16, 0, Some(Weekday::Sat)),
            ChipFace::CruiseUntil,
            "\u{221e} \u{2913}16:00",
            "\u{221e}",
            (Accent, Amber),
            "autopilot: cruise until Sat 16:00, then land \u{2014} keeps the work moving \u{b7} \
             click: land now",
            "autopilot: cruise until Sat 16:00, then land",
            "Autopilot: cruise. Lands Saturday at 16:00.",
        ),
        row(
            AutopilotView::Land,
            ChipGlass::default(),
            ChipFace::Land,
            "\u{2913} land",
            "\u{2913}",
            (Amber, Amber),
            "autopilot: land \u{2014} wrapping up \u{b7} click: off",
            "autopilot: land",
            "Autopilot: land.",
        ),
        row(
            AutopilotView::Land,
            told(TaskFace::Open),
            ChipFace::Land,
            "\u{2913} land",
            "\u{2913}",
            (Amber, Amber),
            "autopilot: land \u{2014} wrapping up \u{b7} click: off",
            "autopilot: land",
            "Autopilot: land.",
        ),
        row(
            AutopilotView::Land,
            told(TaskFace::Landed),
            ChipFace::Landed,
            "\u{2713} landed",
            "\u{2713}",
            (Amber, Amber),
            "autopilot: land, landed \u{2014} the agent finished after the land instruction; \
             the harness types nothing more until someone else's turn \u{b7} click: off",
            "autopilot: land, landed",
            "Autopilot: land. Landed at 17:42.",
        ),
        row(
            AutopilotView::Land,
            told(TaskFace::Done),
            ChipFace::Done,
            "\u{2713} done",
            "\u{2713}",
            (Amber, Amber),
            "autopilot: land, done \u{2014} the agent said it was finished before a landing was \
             asked; the harness types nothing more until someone else's turn \u{b7} click: off",
            "autopilot: land, done",
            "Autopilot: land. Done at 17:42.",
        ),
        row(
            AutopilotView::Land,
            told(TaskFace::Spent),
            ChipFace::Spent,
            "\u{2913} quiet",
            "\u{2913}",
            (Dim, Dim),
            &spent,
            "autopilot: land, quiet",
            &spent_spoken,
        ),
        row(
            AutopilotView::Land,
            told(TaskFace::Theirs),
            ChipFace::Theirs,
            "\u{2913} quiet",
            "\u{2913}",
            (Dim, Dim),
            "autopilot: land, quiet \u{2014} a person's own exchange; the harness types nothing \
             until someone else's turn ends \u{b7} click: off",
            "autopilot: land, quiet",
            "Autopilot: land. Quiet: a person is driving the session.",
        ),
        row(
            AutopilotView::Land,
            ChipGlass {
                held: true,
                ..told(TaskFace::Landed)
            },
            ChipFace::Held,
            "\u{2913} held",
            "\u{2913}",
            (Dim, Dim),
            "autopilot: land, held \u{2014} Codex is pursuing its /goal, and Land types nothing \
             while it runs \u{b7} click: off",
            "autopilot: land, held",
            "Autopilot: land. Held while Codex pursues its goal.",
        ),
        row(
            AutopilotView::Off,
            ChipGlass::default(),
            ChipFace::Off,
            "\u{25cb} off",
            "\u{25cb}",
            (Dim, Dim),
            "autopilot: off \u{2014} hands off \u{b7} click: cruise",
            "autopilot: off",
            "Autopilot: off.",
        ),
        row(
            AutopilotView::MasterOff,
            ChipGlass::default(),
            ChipFace::MasterOff,
            "\u{2298} off",
            "\u{2298}",
            (Dim, Dim),
            "harness off in Settings \u{b7} click: open Settings \u{25b8} Harness",
            "harness off in Settings",
            "Autopilot: off. The harness is off in Settings.",
        ),
        row(
            AutopilotView::Unhosted,
            ChipGlass::default(),
            ChipFace::Unhosted,
            "\u{2298} off",
            "\u{2298}",
            (Dim, Dim),
            "no harness: this aterm has no control socket \u{b7} click: open Settings \u{25b8} \
             Harness",
            "no harness: this aterm has no control socket",
            "Autopilot: off. This aterm has no control socket.",
        ),
    ]
}

/// The ink a tone takes in a row of `blank`'s, under the test accent.
fn ink(tone: ChipTone) -> [u8; 3] {
    match tone {
        ChipTone::Accent => ACCENT,
        ChipTone::Amber => AMBER,
        ChipTone::Dim => dim_of(blank()),
    }
}

/// `view` with `glass` folded into a window's lights, as a frame folds it.
fn lights(view: AutopilotView, glass: ChipGlass) -> WindowLights {
    let mut w = WindowLights::default();
    w.note_autopilot_at(7, view.mode(), view, glass, Instant::now);
    w
}

/// EVERY STATE: each view and glass of §§9.2–9.5 wears its face — drawn in
/// its inks, the timer's `∞` in Cruise's and its `⤓18:00` in Land's — with
/// its whole title, its short title (the mode phrase) and its spoken label;
/// and every face of the table is some state's. A timer shows only under
/// Cruise, a face and a goal hold only under Land.
#[test]
fn the_face_table_gives_every_state_its_face_ink_titles_and_words() {
    let now = Instant::now();
    let mut seen = std::collections::BTreeSet::new();
    for row in rows() {
        let what = format!("{:?} {:?}", row.view, row.glass);
        assert_eq!(ChipFace::of(row.view, &row.glass), row.face, "{what}");
        seen.insert(format!("{:?}", row.face));
        assert_eq!(row.face.tones(), row.tones, "{what}");
        assert_eq!(
            autopilot_title(row.view, &row.glass, "", false),
            (row.whole.clone(), row.short.to_owned()),
            "{what}"
        );
        assert_eq!(row.face.spoken(&row.glass), row.spoken, "{what}");
        if row.view == AutopilotView::Unhosted {
            // No host: neither harness chip is drawn (§2.3).
            let w = lights(row.view, row.glass);
            assert!(
                w.block(7, blank(), now, ACCENT, Keep::Drawn)
                    .lights
                    .is_empty()
            );
            continue;
        }
        let w = lights(row.view, row.glass);
        let block = w.block(7, blank(), now, ACCENT, Keep::Drawn);
        let n = row.drawn.chars().count();
        assert_eq!(
            text(&block.lights),
            format!("{} \u{2699}", row.drawn),
            "{what}"
        );
        assert_eq!(block.cols[0], (0..n, Chip::Autopilot), "{what}");
        let glyph = row.glyph.chars().count();
        let (glyph_ink, word_ink) = (ink(row.tones.0), ink(row.tones.1));
        assert!(
            block.lights[..glyph].iter().all(|c| c.fg == glyph_ink),
            "{what}: the glyph's ink"
        );
        assert!(
            block.lights[glyph..n].iter().all(|c| c.fg == word_ink),
            "{what}: the word's ink"
        );
        // What the fit measures is what the painter lays.
        {
            let s = &w.sessions[&7];
            let on = drawn_at(s, now, None, None, Keep::Drawn);
            assert_eq!(
                chips_width_at(
                    s,
                    Chip::row().filter(|chip| on(*chip)),
                    Keep::Drawn,
                    ACCENT,
                    blank().fg,
                    dim_of(blank())
                ),
                block.lights.len(),
                "{what}"
            );
        }
        // Hovered, the title is the table's.
        let mut w = w;
        w.hover = Some((7, Chip::Autopilot));
        let block = w.block(7, blank(), now, ACCENT, Keep::Drawn);
        assert_eq!(text(&block.title), format!("{}  ", row.whole), "{what}");
        assert_eq!(
            text(&block.short_title),
            format!("{}  ", row.short),
            "{what}"
        );
    }
    assert_eq!(
        seen.len(),
        ChipFace::ALL.len(),
        "every face is a state's: {seen:?}"
    );
    // The timer and the face are glass of their own modes only.
    for view in [
        AutopilotView::Land,
        AutopilotView::Off,
        AutopilotView::MasterOff,
    ] {
        assert_ne!(
            ChipFace::of(view, &timer(18, 0, None)),
            ChipFace::CruiseUntil,
            "{view:?}"
        );
    }
    for view in [
        AutopilotView::Cruise,
        AutopilotView::Off,
        AutopilotView::MasterOff,
    ] {
        let held = ChipGlass {
            held: true,
            ..told(TaskFace::Landed)
        };
        assert_eq!(
            ChipFace::of(view, &held),
            ChipFace::of(view, &ChipGlass::default()),
            "{view:?}: v1's face"
        );
    }
}

/// EACH MODE PHRASE HOLDS NO ` — ` AND NO ` · `: the short title is the
/// phrase whole, with a trail and the menu's line or without them — so a
/// narrow rule shows the mode, and the whole title always keeps the
/// explanation ahead of the trail.
#[test]
fn each_mode_phrase_holds_no_cut_mark() {
    for row in rows() {
        let phrase = row.face.phrase(&row.glass);
        assert!(
            !phrase.contains(" \u{2014} ") && !phrase.contains(" \u{b7} "),
            "{phrase}"
        );
        assert_eq!(phrase, row.short);
        let trail = "\u{2713} approved 14:05, \u{21bb} kept going 14:02";
        for (trail, menu) in [("", true), (trail, false), (trail, true)] {
            let (whole, short) = autopilot_title(row.view, &row.glass, trail, menu);
            assert_eq!(short, row.short, "{whole}");
            assert_eq!(short_title(&whole), row.short, "{whole}");
            assert_eq!(
                whole.ends_with(" \u{b7} right-click: more"),
                menu,
                "{whole}"
            );
            if !trail.is_empty() {
                let at = whole.find(" \u{b7} last: ").expect("the trail");
                let explained = row.face.explanation().map_or(0, |e| {
                    whole.find(e.as_ref()).expect("the explanation") + e.len()
                });
                assert!(explained <= at, "the explanation leads the trail: {whole}");
                assert!(whole.contains(&format!(" \u{b7} last: {trail} \u{b7} click: ")));
            }
        }
    }
}

/// EACH SPOKEN LABEL HOLDS NO GLYPH: letters, digits, spaces and `.:,`
/// alone — the chip's label, and its description of the newest act.
#[test]
fn each_spoken_label_holds_no_glyph() {
    let words = |text: &str| {
        text.chars()
            .all(|c| c.is_ascii_alphanumeric() || matches!(c, ' ' | '.' | ':' | ','))
    };
    for row in rows() {
        assert!(words(&row.spoken), "{}", row.spoken);
        assert!(row.spoken.starts_with("Autopilot: "), "{}", row.spoken);
    }
    for act in Act::ALL {
        let glass = ChipGlass {
            last: Some((act, clock(14, 5))),
            ..ChipGlass::default()
        };
        let said = spoken_last(&glass).expect("an act");
        assert!(words(&said), "{said}");
        assert!(said.ends_with(" at 14:05."), "{said}");
    }
    assert_eq!(spoken_last(&ChipGlass::default()), None);
    // §9.2.1's own words.
    let approved = ChipGlass {
        last: Some((Act::Approved, clock(14, 5))),
        ..ChipGlass::default()
    };
    assert_eq!(
        spoken_last(&approved).as_deref(),
        Some("Last: approved at 14:05.")
    );
    // A face told where the zone could not be read says no time.
    let unplaced = ChipGlass {
        task: Some(TaskFace::Landed),
        ..ChipGlass::default()
    };
    assert_eq!(
        ChipFace::Landed.spoken(&unplaced),
        "Autopilot: land. Landed."
    );
}

/// EACH `GlyphOnly` RUNG is the face's glyph alone, in the glyph's ink —
/// `∞`, `⤓`, `✓`, `○`, `⊘` — with `⚙` and every light given up, and
/// measured as laid out. A click in flight is `◐` there as everywhere.
#[test]
fn each_glyph_only_rung_is_the_glyph_alone() {
    let now = Instant::now();
    for row in rows() {
        if row.view == AutopilotView::Unhosted {
            continue;
        }
        let what = format!("{:?} {:?}", row.view, row.glass);
        let w = lights(row.view, row.glass);
        let block = w.block(7, blank(), now, ACCENT, Keep::GlyphOnly);
        assert_eq!(text(&block.lights), row.glyph, "{what}");
        assert!(
            block.lights.iter().all(|c| c.fg == ink(row.tones.0)),
            "{what}"
        );
        assert!(!block.shows(Chip::Settings), "{what}");
        let s = &w.sessions[&7];
        let on = drawn_at(s, now, None, None, Keep::GlyphOnly);
        assert_eq!(
            chips_width_at(
                s,
                Chip::row().filter(|chip| on(*chip)),
                Keep::GlyphOnly,
                ACCENT,
                blank().fg,
                dim_of(blank())
            ),
            block.lights.len(),
            "{what}"
        );
    }
    let mut w = lights(AutopilotView::Cruise, timer(18, 0, None));
    w.sessions.get_mut(&7).unwrap().autopilot_pending =
        Some((Autopilot::Land, now + Duration::from_secs(1)));
    assert_eq!(
        text(&w.block(7, blank(), now, ACCENT, Keep::GlyphOnly).lights),
        PENDING_GLYPH
    );
    assert_eq!(
        Keep::LADDER.last(),
        Some(&Keep::GlyphOnly),
        "the ladder's last rung"
    );
}

/// A NARROW RULE GIVES A STANDING FACE DOWN TO ITS GLYPH (§9.3.2), and v1's
/// faces as v1 did. Walked from a room that holds both chips down to none,
/// each state's Autopilot chip is its whole face beside `⚙`, then its whole
/// face alone, then — for a standing face of v2's alone (`∞ ⤓18:00`,
/// `✓ landed`, `✓ done`, `⤓ quiet`, `⤓ held`) — its glyph alone, in the
/// glyph's ink and holding the room, down to one cell. v1's faces (`∞`,
/// `⤓ land`, `○ off`, `⊘ off`) give the chip up whole below their width,
/// as v1's ladder did, so the default glass draws nothing new. No chip comes
/// back at a narrower room. CONTROL: the standing faces are exactly v2's.
#[test]
fn a_narrow_rule_gives_a_standing_face_down_to_its_glyph() {
    let now = Instant::now();
    let standing: Vec<ChipFace> = ChipFace::ALL.into_iter().filter(|f| f.standing()).collect();
    assert_eq!(
        standing,
        [
            ChipFace::CruiseUntil,
            ChipFace::Landed,
            ChipFace::Done,
            ChipFace::Spent,
            ChipFace::Theirs,
            ChipFace::Held,
        ],
        "CONTROL: v2's standing faces"
    );
    for view in AutopilotView::ALL {
        assert!(
            !ChipFace::of(view, &ChipGlass::default()).standing(),
            "CONTROL: v1's {view:?} stands on v1's ladder"
        );
    }
    for row in rows() {
        if row.view == AutopilotView::Unhosted {
            continue;
        }
        let what = format!("{:?} {:?}", row.view, row.glass);
        let w = lights(row.view, row.glass);
        let face = row.drawn.chars().count();
        let mut wider = usize::MAX;
        for room in (0..=face + 4).rev() {
            let block = w.fitted(7, blank(), now, ACCENT, None, None, room);
            let drawn = block.as_ref().map(|b| text(&b.lights));
            let want = if room >= face + 2 {
                Some(format!("{} \u{2699}", row.drawn))
            } else if room >= face {
                Some(row.drawn.to_owned())
            } else if room >= 1 && row.face.standing() {
                Some(row.glyph.to_owned())
            } else {
                None
            };
            assert_eq!(drawn, want, "{what} in {room} cells");
            let cells = drawn.map_or(0, |d| d.chars().count());
            assert!(cells <= wider, "{what} in {room} cells: a chip came back");
            wider = cells;
            if let Some(block) = block
                && room < face
            {
                assert!(
                    block.lights.iter().all(|c| c.fg == ink(row.tones.0)),
                    "{what} in {room} cells: the glyph's ink"
                );
                assert_eq!(block.holds, room, "{what} in {room} cells: holds the room");
            }
        }
    }
}

/// THE DEFAULT GLASS IS v1's CHIP: for every view its face, inks, titles
/// (no trail, no menu) and repaint term are v1's, so nothing a person sees
/// moves until a fact is told.
#[test]
fn the_default_glass_is_v1s_chip() {
    let now = Instant::now();
    let rest = ChipGlass::default();
    for view in AutopilotView::ALL {
        let face = ChipFace::of(view, &rest);
        let drawn = match face.word() {
            Some(word) => format!("{} {word}", face.glyph()),
            None => face.glyph().to_owned(),
        };
        assert_eq!(drawn, view.label(), "{view:?}");
        assert_eq!(face.tones(), (view.tone(), view.tone()), "{view:?}");
        assert_eq!(face.clock(&rest), None, "{view:?}");
        let (whole, short) = autopilot_title(view, &rest, "", crate::autopilot_menu::OPENS);
        assert_eq!(whole, view.title(), "{view:?}");
        assert_eq!(short, short_title(view.title()), "{view:?}");
        let mut w = WindowLights::default();
        w.note_autopilot(7, view.mode(), view, now);
        let v1 = |word: Option<Autopilot>| word.map_or(0, |w| 1 + w as u64);
        assert_eq!(
            w.autopilot_term(7, view.mode(), view, rest),
            v1(view.mode()) | (view as u64) << 8,
            "{view:?}"
        );
    }
    assert_eq!(glass_bits(rest, true), 0);
}

/// THE AUTOPILOT TITLE IS SIZED ONCE: built on every frame a hover or a
/// selection is up, a face of v1's costs two allocations — the whole title
/// and the short — as v1's copies of its static titles did, with the menu's
/// line or without it; never one regrowth per part. CONTROL: the parts are
/// all there (the title is v1's).
#[test]
fn the_autopilot_title_is_sized_once() {
    let rest = ChipGlass::default();
    for view in AutopilotView::ALL {
        for menu in [false, true] {
            let ((whole, short), allocations) =
                crate::work_counts::allocations_in(|| autopilot_title(view, &rest, "", menu));
            assert_eq!(allocations, 2, "{view:?} menu={menu}: {whole}");
            assert!(whole.starts_with(view.title()), "CONTROL {view:?}: {whole}");
            assert_eq!(short, short_title(view.title()), "CONTROL {view:?}");
        }
    }
    // A trail and the menu's line are sized in too: one more part, no
    // regrowth.
    let trail = "\u{2713} approved 14:05";
    let ((whole, _), allocations) = crate::work_counts::allocations_in(|| {
        autopilot_title(AutopilotView::Cruise, &rest, trail, true)
    });
    assert_eq!(allocations, 2, "{whole}");
    assert_eq!(whole.capacity(), whole.len(), "sized exactly: {whole}");
}

/// What [`glass_bits`] packed, read back by §9.2.3's table.
#[derive(Debug, PartialEq, Eq)]
struct Unpacked {
    lit: Option<Act>,
    task: Option<TaskFace>,
    held: bool,
    minute: Option<u16>,
    other_day: bool,
    trail: u8,
}

fn unpack(term: u64) -> Unpacked {
    let field = |at: u32, width: u32| (term >> at) & ((1 << width) - 1);
    let lit = match field(24, 8) {
        0 => None,
        n => Some(Act::ALL[usize::try_from(n - 1).unwrap()]),
    };
    let task = match field(32, 3) {
        0 => None,
        1 => Some(TaskFace::Landed),
        2 => Some(TaskFace::Done),
        3 => Some(TaskFace::Spent),
        4 => Some(TaskFace::Theirs),
        n => panic!("no face {n}"),
    };
    let minute = match field(36, 11) {
        0 => None,
        n => Some(u16::try_from(n - 1).unwrap()),
    };
    assert_eq!(
        term & 0xff_ffff,
        0,
        "v1's bits are the word's, view's and click's"
    );
    assert_eq!(term >> 56, 0, "bits 56–63 are spare");
    Unpacked {
        lit,
        task,
        held: field(35, 1) == 1,
        minute,
        other_day: field(47, 1) == 1,
        trail: u8::try_from(field(48, 8)).unwrap(),
    }
}

/// THE BIT TABLE ROUND TRIPS: every act, face, hold, timer minute (the
/// day's first and last among them), other-day bit and trail generation
/// packs into its own bits and reads back — open reading as unknown (both
/// draw Land's own face), and the trail only while the title is read.
#[test]
fn the_autopilot_terms_bit_table_round_trips() {
    let acts = std::iter::once(None).chain(Act::ALL.into_iter().map(Some));
    for lit in acts {
        let faces = std::iter::once(None).chain(TaskFace::ALL.into_iter().map(Some));
        for task in faces {
            for held in [false, true] {
                for at in [
                    None,
                    Some((0, None)),
                    Some((1439, Some(Weekday::Sun))),
                    Some((1080, None)),
                ] {
                    for (trail, reading) in [(0, true), (7, true), (255, true), (9, false)] {
                        let glass = ChipGlass {
                            lit,
                            task,
                            task_at: Some(clock(17, 42)),
                            land_at: at.map(|(minute, other_day)| LandAtGlass {
                                clock: Clock::of_minute(minute),
                                other_day,
                            }),
                            held,
                            last: Some((Act::Approved, clock(14, 5))),
                            trail,
                        };
                        let back = unpack(glass_bits(glass, reading));
                        assert_eq!(
                            back,
                            Unpacked {
                                lit,
                                task: task.filter(|f| *f != TaskFace::Open),
                                held,
                                minute: at.map(|(m, _)| m),
                                other_day: at.is_some_and(|(_, d)| d.is_some()),
                                trail: if reading { trail } else { 0 },
                            },
                            "{glass:?} reading={reading}"
                        );
                    }
                }
            }
        }
    }
    // Under v1's bits, every glass leaves the word, the view and the click.
    let mut w = WindowLights::default();
    w.note_autopilot(
        7,
        Some(Autopilot::Cruise),
        AutopilotView::Cruise,
        Instant::now(),
    );
    let glass = ChipGlass {
        lit: Some(Act::Nudged),
        ..timer(23, 59, Some(Weekday::Mon))
    };
    let term = w.autopilot_term(7, Some(Autopilot::Cruise), AutopilotView::Cruise, glass);
    assert_eq!(
        term & 0xff_ffff,
        w.autopilot_term(
            7,
            Some(Autopilot::Cruise),
            AutopilotView::Cruise,
            ChipGlass::default()
        )
    );
    assert_eq!(term >> 24, glass_bits(glass, false) >> 24);
}

/// THE TRAIL'S GENERATION COUNTS ONLY WHILE THE TITLE IS READ: an act that
/// lights nothing moves the term only for the session whose Autopilot chip
/// is hovered or selected — not another chip of it, not another session.
#[test]
fn the_trail_moves_the_term_only_while_the_chip_is_read() {
    let now = Instant::now();
    let mut w = WindowLights::default();
    w.note_autopilot(7, None, AutopilotView::Cruise, now);
    let term = |w: &WindowLights, trail: u8| {
        let glass = ChipGlass {
            trail,
            ..ChipGlass::default()
        };
        w.autopilot_term(7, None, AutopilotView::Cruise, glass)
    };
    assert_eq!(term(&w, 3), term(&w, 4), "no one reads the title");
    w.hover = Some((7, Chip::Settings));
    assert_eq!(term(&w, 3), term(&w, 4), "another chip's title");
    w.hover = Some((8, Chip::Autopilot));
    assert_eq!(term(&w, 3), term(&w, 4), "another session's chip");
    w.hover = Some((7, Chip::Autopilot));
    assert_ne!(term(&w, 3), term(&w, 4), "the pointer reads it");
    w.hover = None;
    w.selected = Some((7, Chip::Autopilot));
    assert_ne!(term(&w, 3), term(&w, 4), "the keyboard reads it");
}

/// THE ACT'S LABEL is its glyph and word in the chip's ink, and the clock
/// and the weekday are the table's: `HH:MM` both ways, 1970-01-01 a
/// Thursday.
#[test]
fn the_acts_label_and_the_glass_clock_are_the_tables() {
    assert_eq!(
        act_label(AutopilotView::Cruise, Act::Approved),
        ("\u{2713}", "approved", ChipTone::Accent)
    );
    assert_eq!(
        act_label(AutopilotView::Land, Act::Continued),
        ("\u{21bb}", "kept going", ChipTone::Amber)
    );
    for minute in [0, 59, 60, 1080, 1439] {
        assert_eq!(Clock::of_minute(minute).minute(), minute);
    }
    assert_eq!(Clock::of_minute(18 * 60).as_str(), "18:00");
    assert_eq!(Clock::of_minute(7 * 60 + 5).as_str(), "07:05");
    assert_eq!(Clock::of_minute(1440), Clock::of_minute(0), "wraps");
    assert_eq!(Weekday::of_days(0), Weekday::Thu);
    assert_eq!(Weekday::of_days(2), Weekday::Sat);
    assert_eq!(Weekday::of_days(-1), Weekday::Wed);
    assert_eq!(
        (Weekday::Sat.short(), Weekday::Sat.long()),
        ("Sat", "Saturday")
    );
}

/// A CODEX GOAL'S HOLD REACHES THE CHIP THROUGH THE APP's seam
/// (`App::note_codex_goal_hold`, §9.5.4), so the Codex chips never edit the
/// glass's reader: held, the session's glass holds and under Land its face is
/// `⤓ held`, with the repaint term moved; told again it is no move; another
/// session's glass stays v1's. Let go — or its session retired — the glass
/// is v1's again and nothing is kept, so the frame looks nothing up.
/// CONTROL: before any hold, the glass is v1's.
#[test]
fn a_codex_goal_hold_reaches_the_chip_through_the_app() {
    let mut app = App::headless_for_test();
    assert_eq!(app.chip_glass(7), ChipGlass::default(), "CONTROL");
    assert!(app.note_codex_goal_hold(7, true), "a hold is a move");
    assert!(!app.note_codex_goal_hold(7, true), "told again: no move");
    let held = app.chip_glass(7);
    assert!(held.held);
    assert_eq!(ChipFace::of(AutopilotView::Land, &held), ChipFace::Held);
    assert_eq!(app.chip_glass(8), ChipGlass::default(), "another session's");
    let w = WindowLights::default();
    let land = Some(Autopilot::Land);
    assert_ne!(
        w.autopilot_term(7, land, AutopilotView::Land, held),
        w.autopilot_term(7, land, AutopilotView::Land, ChipGlass::default()),
        "the hold moves the repaint term"
    );
    assert!(app.note_codex_goal_hold(7, false), "let go: a move");
    assert!(!app.note_codex_goal_hold(7, false));
    assert_eq!(app.chip_glass(7), ChipGlass::default());
    assert!(app.codex_goal_holds.is_empty());
    app.note_codex_goal_hold(9, true);
    app.retire_session_registration(9);
    assert!(app.codex_goal_holds.is_empty(), "retired with its session");
}
