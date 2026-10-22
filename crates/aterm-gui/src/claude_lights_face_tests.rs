// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! THE FACE TABLE (`docs/DESIGN-harness-autopilot-2026-10-15.md` §9.6.0):
//! every face the Autopilot chip wears for every state of its glass
//! (§§9.2–9.5), its inks, its whole and short titles and its spoken label;
//! its glyph-only rung; the default glass giving v1's chip exactly; and the
//! repaint term's bit table.

use super::*;

use aterm_agent::harness::acts::{Act, TaskFace};

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

/// The local day the table's faces were told on, and the day its titles are
/// read against: a Friday (2024-10-11).
const TODAY: i64 = 20_007;

/// The stamp of `h:m` on [`TODAY`].
fn stamp(h: u16, m: u16) -> Stamp {
    Stamp {
        clock: clock(h, m),
        day: TODAY,
    }
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
        task_at: Some(stamp(17, 42)),
        ..ChipGlass::default()
    }
}

/// The task's face told at 17:42, under a timer landing at `h:m` (on another
/// day where `day` names one).
fn told_under(face: TaskFace, h: u16, m: u16, day: Option<Weekday>) -> ChipGlass {
    ChipGlass {
        land_at: timer(h, m, day).land_at,
        ..told(face)
    }
}

/// One state of the chip: its view and its glass, and what the table gives
/// it — the face as drawn, its glyph-only rung, its inks, its whole title
/// (no trail; the menu's line last, as every Autopilot chip's menu opens),
/// its short title and its spoken label.
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

/// Every state of §§9.2–9.5 and §10.2, in the words of the design; each
/// mode's sentence read from the one table (`autopilot::MODE_WORDS`,
/// §12.4.1), never a copy of its words.
fn rows() -> Vec<Row> {
    use crate::autopilot::mode_words;
    use ChipTone::{Accent, Amber, Dim};
    use aterm_agent::supervise::Autopilot;
    let timed = crate::autopilot::TIMED_CRUISE_CHIP;
    let (cruise, land, off) = (
        mode_words(Autopilot::Cruise).chip,
        mode_words(Autopilot::Land).chip,
        mode_words(Autopilot::Off).chip,
    );
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
    vec![
        row(
            AutopilotView::Cruise,
            ChipGlass::default(),
            ChipFace::Cruise,
            "\u{221e}",
            "\u{221e}",
            (Accent, Accent),
            &format!(
                "autopilot: cruise \u{2014} {cruise} \u{b7} click: land \u{b7} right-click: more"
            ),
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
            &format!(
                "autopilot: cruise until 18:00, then land \u{2014} {timed} \u{b7} click: land now \
                 \u{b7} right-click: more"
            ),
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
            &format!(
                "autopilot: cruise until Sat 16:00, then land \u{2014} {timed} \u{b7} click: land \
                 now \u{b7} right-click: more"
            ),
            "autopilot: cruise until Sat 16:00, then land",
            "Autopilot: cruise. Lands Saturday at 16:00.",
        ),
        // FOREVER CRUISE: Cruise takes nothing as the end, so a landing a
        // loop under Land told keeps `∞` (and, with a timer armed, the
        // timer's face): no task face shows under Cruise.
        row(
            AutopilotView::Cruise,
            told(TaskFace::Landed),
            ChipFace::Cruise,
            "\u{221e}",
            "\u{221e}",
            (Accent, Accent),
            &format!(
                "autopilot: cruise \u{2014} {cruise} \u{b7} click: land \u{b7} right-click: more"
            ),
            "autopilot: cruise",
            "Autopilot: cruise.",
        ),
        row(
            AutopilotView::Cruise,
            told_under(TaskFace::Landed, 16, 0, Some(Weekday::Sat)),
            ChipFace::CruiseUntil,
            "\u{221e} \u{2913}16:00",
            "\u{221e}",
            (Accent, Amber),
            &format!(
                "autopilot: cruise until Sat 16:00, then land \u{2014} {timed} \u{b7} click: land \
                 now \u{b7} right-click: more"
            ),
            "autopilot: cruise until Sat 16:00, then land",
            "Autopilot: cruise. Lands Saturday at 16:00.",
        ),
        // A person's own exchange under Cruise keeps `∞` (§10.2.4), and with
        // a timer armed, the timer's face.
        row(
            AutopilotView::Cruise,
            told(TaskFace::Theirs),
            ChipFace::Cruise,
            "\u{221e}",
            "\u{221e}",
            (Accent, Accent),
            &format!(
                "autopilot: cruise \u{2014} {cruise} \u{b7} click: land \u{b7} right-click: more"
            ),
            "autopilot: cruise",
            "Autopilot: cruise.",
        ),
        row(
            AutopilotView::Cruise,
            told_under(TaskFace::Theirs, 18, 0, None),
            ChipFace::CruiseUntil,
            "\u{221e} \u{2913}18:00",
            "\u{221e}",
            (Accent, Amber),
            &format!(
                "autopilot: cruise until 18:00, then land \u{2014} {timed} \u{b7} click: land now \
                 \u{b7} right-click: more"
            ),
            "autopilot: cruise until 18:00, then land",
            "Autopilot: cruise. Lands at 18:00.",
        ),
        row(
            AutopilotView::Land,
            ChipGlass::default(),
            ChipFace::Land,
            "\u{2913} land",
            "\u{2913}",
            (Amber, Amber),
            &format!("autopilot: land \u{2014} {land} \u{b7} click: off \u{b7} right-click: more"),
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
            &format!("autopilot: land \u{2014} {land} \u{b7} click: off \u{b7} right-click: more"),
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
            "autopilot: land, landed 17:42 \u{2014} all of the work is on the remote main \
             branch; the harness types nothing more until someone else's turn \u{b7} click: off \
             \u{b7} right-click: more",
            "autopilot: land, landed 17:42",
            "Autopilot: land. Landed at 17:42.",
        ),
        row(
            AutopilotView::Land,
            told(TaskFace::Theirs),
            ChipFace::Theirs,
            "\u{2913} quiet",
            "\u{2913}",
            (Dim, Dim),
            "autopilot: land, quiet \u{2014} a person's own exchange; the harness types nothing \
             until someone else's turn ends \u{b7} click: off \u{b7} right-click: more",
            "autopilot: land, quiet",
            "Autopilot: land. Quiet: a person is driving the session.",
        ),
        row(
            AutopilotView::Off,
            ChipGlass::default(),
            ChipFace::Off,
            "\u{25cb} off",
            "\u{25cb}",
            (Dim, Dim),
            &format!("autopilot: off \u{2014} {off} \u{b7} click: cruise \u{b7} right-click: more"),
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
            "harness off in Settings \u{b7} click: turn the harness on \u{b7} right-click: more",
            "harness off in Settings",
            "Autopilot: off. The harness is off in Settings.",
        ),
        // §10.2.1: the master on and an agent's own switch off names the
        // switch, as its Settings row does.
        row(
            AutopilotView::AgentOff(aterm_phase::Program::Claude),
            ChipGlass::default(),
            ChipFace::AgentOff(aterm_phase::Program::Claude),
            "\u{2298} off",
            "\u{2298}",
            (Dim, Dim),
            "Supervise Claude Code sessions is off \u{b7} click: turn the harness on \u{b7} \
             right-click: more",
            "Supervise Claude Code sessions is off",
            "Autopilot: off. Supervise Claude Code sessions is off in Settings.",
        ),
        row(
            AutopilotView::Unhosted,
            ChipGlass::default(),
            ChipFace::Unhosted,
            "\u{2298} off",
            "\u{2298}",
            (Dim, Dim),
            "no harness: this aterm has no control socket \u{b7} click: open Settings \u{25b8} \
             Harness \u{b7} right-click: more",
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
        assert_eq!(row.face.rung(), row.glyph, "{what}: the glyph-only rung");
        seen.insert(format!("{:?}", row.face));
        assert_eq!(row.face.tones(), row.tones, "{what}");
        assert_eq!(
            autopilot_title(row.view, &row.glass, "", Some(TODAY), TitleWords::default()),
            (row.whole.clone(), row.short.to_owned()),
            "{what}"
        );
        assert_eq!(
            row.face.spoken(&row.glass, Some(TODAY)),
            row.spoken,
            "{what}"
        );
        if row.view == AutopilotView::Unhosted {
            // No host: no Autopilot chip is drawn (§2.3).
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
        assert_eq!(text(&block.lights), row.drawn, "{what}");
        assert_eq!(block.cols[0], (0..n, Chip::Autopilot), "{what}");
        let glyph = row.face.glyph().chars().count();
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
                    Inks::of(blank(), ACCENT)
                ),
                block.lights.len(),
                "{what}"
            );
        }
        // Hovered, the title is the table's.
        let mut w = w;
        w.hover = Some((7, Target::from(Chip::Autopilot)));
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
    // The task's face is Land's and Cruise's (§10.2), never Off's, the
    // master switch's or an agent's own switch's.
    for view in [
        AutopilotView::Off,
        AutopilotView::MasterOff,
        AutopilotView::AgentOff(aterm_phase::Program::Claude),
    ] {
        for face in TaskFace::ALL {
            assert_eq!(
                ChipFace::of(view, &told(face)),
                ChipFace::of(view, &ChipGlass::default()),
                "{view:?} {face:?}: v1's face"
            );
        }
    }
}

/// CRUISE NEVER RESTS: over view × task face × timer, under Cruise every
/// face — a landing a loop under Land told, a person's own exchange, open,
/// unknown — reads `Cruise`, or `CruiseUntil` with the timer: Cruise takes
/// nothing as the end. Land's faces are open, landed and theirs, the timer
/// no part of them. NEGATIVE CONTROL: the same landed glass under Land
/// wears `✓ landed`.
#[test]
fn under_cruise_no_task_face_shows_and_land_wears_its_own() {
    let faces = std::iter::once(None).chain(TaskFace::ALL.into_iter().map(Some));
    for task in faces {
        for armed in [false, true] {
            let glass = ChipGlass {
                task,
                task_at: Some(stamp(17, 42)),
                land_at: armed.then(|| timer(18, 0, None).land_at).flatten(),
                ..ChipGlass::default()
            };
            let cruise = if armed {
                ChipFace::CruiseUntil
            } else {
                ChipFace::Cruise
            };
            assert_eq!(
                ChipFace::of(AutopilotView::Cruise, &glass),
                cruise,
                "{task:?} armed={armed}"
            );
            let land = match task {
                Some(TaskFace::Landed) => ChipFace::Landed,
                Some(TaskFace::Theirs) => ChipFace::Theirs,
                Some(TaskFace::Open) | None => ChipFace::Land,
            };
            assert_eq!(
                ChipFace::of(AutopilotView::Land, &glass),
                land,
                "{task:?} armed={armed}"
            );
        }
    }
    assert_eq!(
        ChipFace::of(AutopilotView::Land, &told(TaskFace::Landed)),
        ChipFace::Landed,
        "NEGATIVE CONTROL"
    );
}

/// EVERY FACE CLOCK NAMES ITS DAY WHEN IT IS NOT TODAY (§10.2.3 items 3
/// and 4; §10.2.5, "The day"): read against the day after the faces were
/// told, each task's end carries its weekday in the whole title, the short
/// title (`autopilot: land, landed Fri 17:42`) and the spoken label
/// (`Landed on Friday at 17:42.`); read against its own day, or with no day
/// read at all (a title laid with no reading), none does — the clock stands.
/// NEGATIVE CONTROL: a face with no clock names no day either way.
#[test]
fn every_face_clock_names_its_day_when_it_is_not_today() {
    let tomorrow = Some(TODAY + 1);
    for (view, task, word) in [(AutopilotView::Land, TaskFace::Landed, "landed")] {
        let glass = told(task);
        let face = ChipFace::of(view, &glass);
        let what = format!("{view:?} {task:?}");
        let (whole, short) = autopilot_title(view, &glass, "", tomorrow, TitleWords::default());
        assert!(
            short.ends_with(&format!("{word} Fri 17:42")),
            "{what}: {short}"
        );
        assert!(whole.starts_with(&short), "{what}: {whole}");
        let said = face.spoken(&glass, tomorrow);
        assert!(said.ends_with(" on Friday at 17:42."), "{what}: {said}");
        for today in [Some(TODAY), None] {
            let (whole, short) = autopilot_title(view, &glass, "", today, TitleWords::default());
            assert!(
                short.ends_with(&format!("{word} 17:42")),
                "{what} {today:?}: {short}"
            );
            assert!(!whole.contains("Fri"), "{what} {today:?}: {whole}");
            let said = face.spoken(&glass, today);
            assert!(
                said.ends_with(" at 17:42.") && !said.contains("Friday"),
                "{said}"
            );
        }
    }
    // NEGATIVE CONTROL: no clock, no day.
    let quiet = told(TaskFace::Theirs);
    let (whole, short) = autopilot_title(
        AutopilotView::Land,
        &quiet,
        "",
        tomorrow,
        TitleWords::default(),
    );
    assert!(!whole.contains("Fri"), "{whole}");
    assert_eq!(short, "autopilot: land, quiet");
}

/// EACH MODE PHRASE HOLDS NO ` — ` AND NO ` · `: the short title is the
/// phrase whole, with a trail or without one, the menu's line always last —
/// so a narrow rule shows the mode, and the whole title always keeps the
/// explanation ahead of the trail and says the menu is there (§9.3.2).
#[test]
fn each_mode_phrase_holds_no_cut_mark() {
    for row in rows() {
        let phrase = row.face.phrase(&row.glass, Some(TODAY));
        assert!(
            !phrase.contains(" \u{2014} ") && !phrase.contains(" \u{b7} "),
            "{phrase}"
        );
        assert_eq!(phrase, row.short);
        let trail = "\u{2713} approved 14:05, \u{21bb} kept going 14:02";
        for trail in ["", trail] {
            let (whole, short) = autopilot_title(
                row.view,
                &row.glass,
                trail,
                Some(TODAY),
                TitleWords::default(),
            );
            assert_eq!(short, row.short, "{whole}");
            assert_eq!(short_title(&whole), row.short, "{whole}");
            assert!(whole.ends_with(" \u{b7} right-click: more"), "{whole}");
            assert!(
                !short.contains("right-click"),
                "the short form gives the clicks up: {short}"
            );
            if !trail.is_empty() {
                let at = whole.find(" \u{b7} last: ").expect("the trail");
                let explained = row
                    .face
                    .explanation(&row.glass, TitleWords::default(), Some(TODAY))
                    .map_or(0, |e| {
                        whole.find(e.as_ref()).expect("the explanation") + e.len()
                    });
                assert!(explained <= at, "the explanation leads the trail: {whole}");
                assert!(whole.contains(&format!(" \u{b7} last: {trail} \u{b7} click: ")));
            }
            // THE LEANER FORMS (finding 2 of the wiring's review): the whole
            // without its trail, where it has one, then v1's words — the
            // whole with neither the trail nor the menu's line — each wider
            // than the short form and narrower than the one before.
            let [no_trail, words, clicks, click] =
                lean_titles(&whole).map(|(head, tail)| format!("{head}{tail}"));
            let (bare, _) =
                autopilot_title(row.view, &row.glass, "", Some(TODAY), TitleWords::default());
            if trail.is_empty() {
                assert_eq!(no_trail, "", "no trail, no form without one: {whole}");
            } else {
                assert_eq!(no_trail, bare, "the trail given up: {whole}");
                assert!(no_trail.len() < whole.len());
            }
            assert_eq!(
                Some(words.as_str()),
                bare.strip_suffix(" \u{b7} right-click: more"),
                "the menu's line given up too: {whole}"
            );
            assert!(words.starts_with(row.short) && words.len() > row.short.len());
            assert!(
                words.contains(" \u{b7} click: "),
                "the click stays: {words}"
            );
            assert_eq!(short_title(&words), row.short, "{words}");
            // THE CLICKS OUTLAST THE EXPLANATION (owner-eye-1 of the
            // post-landing check): the phrase with both clicks, where that
            // is narrower than v1's words, then the phrase with the click —
            // each narrower than the form before it and wider than the
            // short form; none where the title has no explanation.
            let explained = row
                .face
                .explanation(&row.glass, TitleWords::default(), Some(TODAY));
            let click_words = format!(" \u{b7} click: {}", row.face.click(&row.glass));
            match explained {
                None => assert_eq!(
                    (clicks.as_str(), click.as_str()),
                    ("", ""),
                    "no explanation, nothing to give up: {whole}"
                ),
                Some(e) => {
                    assert_eq!(click, format!("{}{click_words}", row.short), "{whole}");
                    let cells = |t: &str| t.chars().count();
                    if cells(&e) + 3 > cells(" \u{b7} right-click: more") {
                        assert_eq!(
                            clicks,
                            format!("{click} \u{b7} right-click: more"),
                            "{whole}"
                        );
                        assert!(cells(&clicks) < cells(&words), "{whole}");
                        assert!(cells(&click) < cells(&clicks), "{whole}");
                    } else {
                        assert_eq!(clicks, "", "no narrower than v1's words: {whole}");
                        assert!(cells(&click) < cells(&words), "{whole}");
                    }
                    assert!(cells(&click) > cells(row.short), "{whole}");
                    assert!(!click.contains(" \u{2014} "), "{click}");
                    assert_eq!(short_title(&click), row.short, "{click}");
                }
            }
        }
    }
    // NEGATIVE CONTROL: a title that is no Autopilot whole title has none.
    assert_eq!(
        lean_titles("worker of lab-sweep \u{b7} task working 4m \u{2014} click to go there"),
        [("", ""); LEAN_FORMS]
    );
    assert_eq!(lean_titles("autopilot: cruise"), [("", ""); LEAN_FORMS]);
}

/// EVERY FACE KEEPS ITS CLICKS AFTER ITS EXPLANATION (owner-eye-1 of the
/// post-landing check of Autopilot v3 wave 1, Claude Code 2.1.302: hovering
/// `∞ ✓ done` on a 160-column rule showed `autopilot: cruise, done 13:04`
/// alone, its explanation and both clicks gone below about 240 columns,
/// where main before wave 1 kept `click: land` from 120). For every face of
/// the table, with a trail and without one, the hovered block laid as a
/// frame lays it and fitted beside the whole facts (`◆ Opus 5.5 xhigh   ⌂
/// ~/aterm   ⎇ main`) over every room from 400 cells down: the title is the
/// whole, then without its trail (where it has one), then v1's words, then
/// the phrase with both clicks (where that is narrower), then the phrase
/// with the click, then the phrase alone, then none — in that order, never
/// back; each form stands from exactly the room the one before it no
/// longer fits in down to exactly its own width; the facts stay whole
/// wherever a title is drawn; and every form wider than the phrase keeps `·
/// click: `. Then the landed face's own rule at 160, 120, 110 and 90 columns
/// (the finding's face was Cruise's `done`, retired under FOREVER CRUISE).
///
/// NEGATIVE CONTROL: the same block with the two forms taken out (their
/// widths 0, the ladder main had) goes from v1's words straight to the
/// phrase, and the landed face's 160-column rule shows no click.
#[test]
fn every_face_keeps_its_clicks_after_its_explanation() {
    use aterm_agent::harness::footer::{self, FooterFacts, LightsWidth, TitleFit};
    let now = Instant::now();
    let facts = FooterFacts {
        model: Some("Opus 5.5".into()),
        effort: Some("xhigh".into()),
        path: Some("~/aterm".into()),
        branch: Some("main".into()),
        ..FooterFacts::default()
    };
    let whole_facts = footer::fit_rule(&facts, 400, None).segments;
    let hovered = |row: &Row, trail: &str| {
        let (whole, short) = autopilot_title(
            row.view,
            &row.glass,
            trail,
            Some(TODAY),
            TitleWords::default(),
        );
        let w = lights(row.view, row.glass);
        let mut block = Block::default();
        assert!(w.fit_into(
            7,
            blank(),
            now,
            ACCENT,
            None,
            Some(Chip::Autopilot),
            400,
            (Some((whole.as_str(), short.as_str())), None, None),
            &mut block,
        ));
        let widths = LightsWidth {
            chips: block.holds,
            title: block.title.len(),
            lean: block.lean.each_ref().map(Vec::len),
            short: block.short_title.len(),
            reason: block.reason,
        };
        (block, widths)
    };
    let laid = |block: &Block, title: TitleFit| match title {
        TitleFit::Full => text(&block.title),
        TitleFit::Lean(n) => text(&block.lean[usize::from(n)]),
        TitleFit::Short => text(&block.short_title),
        TitleFit::None => String::new(),
    };
    // The forms a walk meets, widest first, and the narrowest room each
    // stood at.
    let walk = |widths: LightsWidth| {
        let mut met: Vec<(TitleFit, usize)> = Vec::new();
        for room in (0..=400).rev() {
            let fit = footer::fit_rule(&facts, room, Some(widths));
            if !fit.chips {
                break;
            }
            match met.last_mut() {
                Some((title, narrowest)) if *title == fit.title => *narrowest = room,
                _ => met.push((fit.title, room)),
            }
            if fit.title != TitleFit::None {
                assert_eq!(fit.segments, whole_facts, "{room}: the facts stay whole");
            }
        }
        met
    };
    for row in rows() {
        if row.view == AutopilotView::Unhosted {
            // No Autopilot chip is drawn in Claude Code's rule with no host.
            continue;
        }
        let what = format!("{:?} {:?}", row.view, row.glass);
        for trail in ["", "\u{2713} approved 14:05"] {
            let (block, widths) = hovered(&row, trail);
            let explained = row
                .face
                .explanation(&row.glass, TitleWords::default(), Some(TODAY))
                .is_some();
            let both = widths.lean[2] > 0;
            let mut want = vec![TitleFit::Full];
            if !trail.is_empty() {
                want.push(TitleFit::Lean(0));
            }
            want.push(TitleFit::Lean(1));
            if both {
                want.push(TitleFit::Lean(2));
            }
            if explained {
                want.push(TitleFit::Lean(3));
            }
            want.extend([TitleFit::Short, TitleFit::None]);
            let met = walk(widths);
            assert_eq!(
                met.iter().map(|(t, _)| *t).collect::<Vec<_>>(),
                want,
                "{what} {trail:?}: the forms in order"
            );
            for &(title, narrowest) in &met {
                if title == TitleFit::None {
                    continue;
                }
                let fit = footer::fit_rule(&facts, narrowest, Some(widths));
                assert_eq!(
                    fit.width(Some(widths)),
                    narrowest,
                    "{what} {trail:?}: {title:?} stands down to its own width"
                );
                let drawn = laid(&block, title);
                assert_eq!(
                    drawn.contains(" \u{b7} click: "),
                    title != TitleFit::Short,
                    "{what} {trail:?}: {title:?} keeps the click: {drawn:?}"
                );
                if title == TitleFit::Lean(2) {
                    assert!(drawn.ends_with("right-click: more  "), "{drawn:?}");
                    assert!(!drawn.contains(" \u{2014} "), "{drawn:?}");
                }
            }
            // NEGATIVE CONTROL: the ladder main had.
            let main = LightsWidth {
                lean: [widths.lean[0], widths.lean[1], 0, 0],
                ..widths
            };
            let before: Vec<TitleFit> = walk(main).into_iter().map(|(t, _)| t).collect();
            assert!(
                !before.contains(&TitleFit::Lean(2)) && !before.contains(&TitleFit::Lean(3)),
                "CONTROL {what}: {before:?}"
            );
        }
    }
    // The finding's own rule, on the face that took the retired Cruise
    // done's place as an end with a time: Land's landing, hovered.
    let landed = rows()
        .into_iter()
        .find(|r| r.face == ChipFace::Landed && r.glass.land_at.is_none())
        .expect("the landed face");
    let (block, widths) = hovered(&landed, "");
    let rule = |cols: usize, widths: LightsWidth| {
        let fit = footer::fit_rule(&facts, footer::rule_room(&(0..cols)), Some(widths));
        laid(&block, fit.title)
    };
    assert_eq!(
        rule(160, widths),
        "autopilot: land, landed 17:42 \u{b7} click: off \u{b7} right-click: more  "
    );
    assert_eq!(rule(120, widths), rule(160, widths), "both clicks at 120");
    assert_eq!(
        rule(110, widths),
        "autopilot: land, landed 17:42 \u{b7} click: off  ",
        "the click at 110 columns"
    );
    assert_eq!(
        rule(90, widths),
        "autopilot: land, landed 17:42  ",
        "the phrase alone at 90"
    );
    let from = |title: TitleFit| {
        (40..=400)
            .filter(|cols| {
                footer::fit_rule(&facts, footer::rule_room(&(0..*cols)), Some(widths)).title
                    == title
            })
            .min()
    };
    eprintln!(
        "the landed face: whole from {:?} columns, v1's words from {:?}, both clicks from {:?}, \
         the click from {:?}, the phrase from {:?}",
        from(TitleFit::Full),
        from(TitleFit::Lean(1)),
        from(TitleFit::Lean(2)),
        from(TitleFit::Lean(3)),
        from(TitleFit::Short)
    );
    let main = LightsWidth {
        lean: [widths.lean[0], widths.lean[1], 0, 0],
        ..widths
    };
    assert_eq!(
        rule(160, main),
        "autopilot: land, landed 17:42  ",
        "CONTROL: main's ladder gives the clicks up"
    );
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
        for day in [None, Some(Weekday::Thu)] {
            let said = spoken_last(act, clock(14, 5), day);
            assert!(words(&said), "{said}");
            assert!(said.ends_with(" at 14:05."), "{said}");
        }
    }
    // §9.2.1's own words, and an act from another day with its weekday.
    assert_eq!(
        spoken_last(Act::Approved, clock(14, 5), None),
        "Last: approved at 14:05."
    );
    assert_eq!(
        spoken_last(Act::Approved, clock(23, 58), Some(Weekday::Thu)),
        "Last: approved on Thursday at 23:58."
    );
    // A face told where the zone could not be read says no time.
    let unplaced = ChipGlass {
        task: Some(TaskFace::Landed),
        ..ChipGlass::default()
    };
    assert_eq!(
        ChipFace::Landed.spoken(&unplaced, Some(TODAY)),
        "Autopilot: land. Landed."
    );
}

/// EACH `GlyphOnly` RUNG is the face's rung alone, in the glyph's ink —
/// `∞`, `⤓`, `✓`, `○`, `⊘` — with every other chip given up, and measured
/// as laid out. A click in flight is `◐` there as everywhere. CONTROL: no
/// face has a rung apart from its glyph (Cruise wears no task's end).
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
        assert!(
            block.cols.iter().all(|(_, chip)| *chip == Chip::Autopilot),
            "{what}: Autopilot alone"
        );
        let s = &w.sessions[&7];
        let on = drawn_at(s, now, None, None, Keep::GlyphOnly);
        assert_eq!(
            chips_width_at(
                s,
                Chip::row().filter(|chip| on(*chip)),
                Keep::GlyphOnly,
                Inks::of(blank(), ACCENT)
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
    let apart: Vec<ChipFace> = ChipFace::ALL
        .into_iter()
        .filter(|face| face.rung() != face.glyph())
        .collect();
    assert!(
        apart.is_empty(),
        "every face's rung is its glyph: {apart:?}"
    );
}

/// A NARROW RULE GIVES A STANDING FACE DOWN TO ITS GLYPH (§9.3.2), and v1's
/// faces as v1 did. Walked from a room that holds the chip down to none,
/// each state's Autopilot chip is its whole face, then — for a standing face
/// of v2's alone (`∞ ⤓18:00`, `✓ landed`, `⤓ quiet`) — its glyph alone, in the
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
        [ChipFace::CruiseUntil, ChipFace::Landed, ChipFace::Theirs],
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
            let want = if room >= face {
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
/// (no trail: v1's words, then the menu's line, §9.2.1) and repaint term
/// are v1's, so nothing a person sees moves until a fact is told but the
/// title's offer of the menu.
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
        let (whole, short) = autopilot_title(view, &rest, "", None, TitleWords::default());
        assert_eq!(
            whole,
            format!("{} \u{b7} right-click: more", view.title()),
            "{view:?}"
        );
        assert_eq!(short, short_title(&view.title()), "{view:?}");
        let [_, (head, tail), ..] = lean_titles(&whole);
        assert_eq!(
            format!("{head}{tail}"),
            view.title(),
            "{view:?}: v1's words"
        );
        let mut w = WindowLights::default();
        w.note_autopilot(7, view.mode(), view, now);
        let v1 = |word: Option<Autopilot>| word.map_or(0, |w| 1 + w as u64);
        assert_eq!(
            w.autopilot_term(7, view.mode(), view, rest),
            v1(view.mode()) | view.code() << 8,
            "{view:?}"
        );
    }
    assert_eq!(glass_bits(rest, true), 0);
}

/// THE AUTOPILOT TITLE IS SIZED ONCE: built on every frame a hover or a
/// selection is up, a face of v1's costs two allocations — the whole title
/// and the short — as v1's copies of its static titles did, the menu's line
/// sized in; never one regrowth per part. CONTROL: the parts are all there
/// (the title is v1's, then the menu's line).
#[test]
fn the_autopilot_title_is_sized_once() {
    let rest = ChipGlass::default();
    for view in AutopilotView::ALL {
        let ((whole, short), allocations) = crate::work_counts::allocations_in(|| {
            autopilot_title(view, &rest, "", None, TitleWords::default())
        });
        assert_eq!(allocations, 2, "{view:?}: {whole}");
        assert!(
            whole.starts_with(&view.title()),
            "CONTROL {view:?}: {whole}"
        );
        assert!(whole.ends_with("right-click: more"), "CONTROL {view:?}");
        assert_eq!(whole.capacity(), whole.len(), "sized exactly: {whole}");
        assert_eq!(short, short_title(&view.title()), "CONTROL {view:?}");
    }
    // A trail is sized in too: one more part, no regrowth.
    let trail = "\u{2713} approved 14:05";
    let ((whole, _), allocations) = crate::work_counts::allocations_in(|| {
        autopilot_title(
            AutopilotView::Cruise,
            &rest,
            trail,
            None,
            TitleWords::default(),
        )
    });
    assert_eq!(allocations, 2, "{whole}");
    assert_eq!(whole.capacity(), whole.len(), "sized exactly: {whole}");
}

/// What [`glass_bits`] packed, read back by §9.2.3's table.
#[derive(Debug, PartialEq, Eq)]
struct Unpacked {
    lit: Option<Act>,
    task: Option<TaskFace>,
    minute: Option<u16>,
    other_day: bool,
    trail: u8,
    weekday: Option<Weekday>,
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
    assert_eq!(term >> 59, 0, "bits 59–63 are spare");
    assert_eq!(
        field(35, 1),
        0,
        "bit 35 is spare (the retired Codex goal hold)"
    );
    Unpacked {
        lit,
        task,
        minute,
        other_day: field(47, 1) == 1,
        trail: u8::try_from(field(48, 8)).unwrap(),
        weekday: match field(56, 3) {
            0 => None,
            n => Some(
                [
                    Weekday::Mon,
                    Weekday::Tue,
                    Weekday::Wed,
                    Weekday::Thu,
                    Weekday::Fri,
                    Weekday::Sat,
                    Weekday::Sun,
                ][usize::try_from(n - 1).unwrap()],
            ),
        },
    }
}

/// THE BIT TABLE ROUND TRIPS: every act, face, timer minute (the
/// day's first and last among them), other-day bit, trail generation and
/// other day's weekday (bits 56–58, 2c74954acc) packs into its own bits
/// and reads back — open reading as unknown (both
/// draw Land's own face), and the trail only while the title is read.
#[test]
fn the_autopilot_terms_bit_table_round_trips() {
    let acts = std::iter::once(None).chain(Act::ALL.into_iter().map(Some));
    for lit in acts {
        let faces = std::iter::once(None).chain(TaskFace::ALL.into_iter().map(Some));
        for task in faces {
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
                        task_at: Some(stamp(17, 42)),
                        land_at: at.map(|(minute, other_day)| LandAtGlass {
                            clock: Clock::of_minute(minute),
                            other_day,
                        }),
                        last: Some((Act::Approved, stamp(14, 5))),
                        trail,
                        pause: None,
                    };
                    let back = unpack(glass_bits(glass, reading));
                    assert_eq!(
                        back,
                        Unpacked {
                            lit,
                            task: task.filter(|f| *f != TaskFace::Open),
                            minute: at.map(|(m, _)| m),
                            other_day: at.is_some_and(|(_, d)| d.is_some()),
                            trail: if reading { trail } else { 0 },
                            weekday: at.and_then(|(_, d)| d),
                        },
                        "{glass:?} reading={reading}"
                    );
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
    w.hover = Some((7, Target::from(Chip::Light(Light::Mode))));
    assert_eq!(term(&w, 3), term(&w, 4), "another chip's title");
    w.hover = Some((8, Target::from(Chip::Autopilot)));
    assert_eq!(term(&w, 3), term(&w, 4), "another session's chip");
    w.hover = Some((7, Target::from(Chip::Autopilot)));
    assert_ne!(term(&w, 3), term(&w, 4), "the pointer reads it");
    w.hover = None;
    w.selected = Some((7, Target::from(Chip::Autopilot)));
    assert_ne!(term(&w, 3), term(&w, 4), "the keyboard reads it");
}

/// A FACE THAT KEEPS ITS KIND AND MOVES ITS CLOCK REPAINTS A TITLE BEING
/// READ (§10.2, slice 1c: the title carries the face's clock, `landed 17:42`
/// then `landed 18:05`): the footer's term moves while the Autopilot chip is
/// hovered, and not while no one reads its title, which a frame at rest
/// does not draw.
///
/// NEGATIVE CONTROL: the moved glass with the trail's generation where it
/// was leaves the term being read where it was — the clock has no bits of
/// its own, so without the generation's move the title kept `17:42`.
#[test]
fn a_face_clock_that_moves_under_its_face_repaints_the_title_being_read() {
    let mut app = App::headless_for_test();
    let mut w = WindowLights::default();
    let term =
        |w: &WindowLights, glass: ChipGlass| w.autopilot_term(7, None, AutopilotView::Land, glass);
    assert!(crate::harness_glass::on_task(
        &mut app,
        7,
        Some(TaskFace::Landed),
        1_792_249_200_000
    ));
    let before = app.chip_glass(7);
    let at_rest = term(&w, before);
    w.hover = Some((7, Target::from(Chip::Autopilot)));
    let read = term(&w, before);
    assert!(crate::harness_glass::on_task(
        &mut app,
        7,
        Some(TaskFace::Landed),
        1_792_250_700_000
    ));
    let after = app.chip_glass(7);
    assert_eq!(after.task, before.task, "the same face");
    assert_ne!(after.task_at, before.task_at, "its clock moved");
    assert_ne!(term(&w, after), read, "the title being read is built again");
    w.hover = None;
    assert_eq!(
        term(&w, after),
        at_rest,
        "no one reads it: nothing repaints"
    );
    // NEGATIVE CONTROL: the clock alone moves no bit.
    w.hover = Some((7, Target::from(Chip::Autopilot)));
    let clock_alone = ChipGlass {
        trail: before.trail,
        ..after
    };
    assert_eq!(term(&w, clock_alone), read);
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

/// LAND'S AMBER CLEARS THE TEXT FLOOR ON EVERY BUILT-IN THEME (the surfaces
/// hunt's F2: `⤓ land` was the fixed `#E8B03C`, 1.88:1 on Aterm Day's
/// `#f8fafd`): drawn on each built-in scheme's own ground, every amber cell
/// — Land's `⤓ land`, its `✓ done`, a timed Cruise's `⤓18:00` and Land's act
/// label — clears 4.5:1, the floor chrome's warn ink takes on the same
/// themes (`chrome_band`'s `band_colors_meet_wcag_aa_on_every_builtin_scheme`);
/// where the amber already clears it (the dark themes) it is the amber
/// itself, the auto pill's hue. NEGATIVE CONTROL: the fixed amber is under
/// the floor on some built-in ground, Aterm Day's among them.
#[test]
fn lands_amber_clears_the_text_floor_on_every_builtin_theme() {
    use crate::chrome_band::contrast;
    let now = Instant::now();
    let rgb = |c: u32| [(c >> 16) as u8, (c >> 8) as u8, c as u8];
    let mut under = Vec::new();
    for name in aterm_types::scheme::builtin_names() {
        let scheme = aterm_types::scheme::builtin(name).expect("a listed scheme exists");
        let theme = aterm_render::Theme::from(&scheme);
        let ground = rgb(theme.bg);
        let row = RenderCell {
            ch: ' ',
            fg: rgb(theme.fg),
            bg: ground,
            ..RenderCell::default()
        };
        if contrast(AMBER, ground) < CHIP_INK_FLOOR {
            under.push(name);
        }
        let floored = land_ink(ground);
        assert!(
            contrast(floored, ground) >= CHIP_INK_FLOOR,
            "{name}: {floored:?} on {ground:?}"
        );
        if contrast(AMBER, ground) >= CHIP_INK_FLOOR {
            assert_eq!(floored, AMBER, "{name}: the amber itself where it clears");
        }
        for (view, glass, amber) in [
            (AutopilotView::Land, ChipGlass::default(), 0..6),
            (AutopilotView::Land, told(TaskFace::Landed), 0..8),
            (AutopilotView::Cruise, timer(18, 0, None), 2..8),
        ] {
            let w = lights(view, glass);
            let block = w.block(7, row, now, ACCENT, Keep::Drawn);
            let cells = &block.lights[amber.clone()];
            assert!(
                cells.iter().all(|c| c.fg == floored),
                "{name} {view:?}: {:?}",
                text(cells)
            );
        }
        // Land's act label, in the chip's ink.
        let mut w = lights(
            AutopilotView::Land,
            ChipGlass {
                lit: Some(Act::Approved),
                ..ChipGlass::default()
            },
        );
        w.hover = None;
        let block = w.block(7, row, now, ACCENT, Keep::Drawn);
        let label: Vec<&RenderCell> = block.title.iter().filter(|c| c.ch != ' ').collect();
        assert!(!label.is_empty(), "{name}: the act's label");
        assert!(
            label.iter().all(|c| c.fg == floored),
            "{name}: the act's label in Land's ink"
        );
    }
    assert!(
        under.contains(&"Aterm Day"),
        "NEGATIVE CONTROL: the fixed amber fails a light ground: {under:?}"
    );
}

/// THE PERMISSION LIGHT'S OWN HUE CLEARS THE SAME FLOOR (the honest chip's
/// review, finding 4: F2's defect on the light beside Land's chip): the mode
/// light in Claude Code's bypass red or auto amber — drawn at rest only where
/// it deviates, and in those hues where it is hovered or selected, just
/// switched, refused or revealed — is floored on each built-in scheme's own
/// ground as Land's amber is, its glyph and its word alike, and is the hue
/// itself wherever that already clears 4.5:1 (every dark theme).
///
/// NEGATIVE CONTROL: both fixed hues are under the floor on Aterm Day's
/// ground (`#e8b03c` 1.88:1, `#e05a5a` 3.47:1).
#[test]
fn the_lights_own_hue_clears_the_text_floor_on_every_builtin_theme() {
    use crate::chrome_band::contrast;
    let now = Instant::now();
    let rgb = |c: u32| [(c >> 16) as u8, (c >> 8) as u8, c as u8];
    let mut under = Vec::new();
    for name in aterm_types::scheme::builtin_names() {
        let scheme = aterm_types::scheme::builtin(name).expect("a listed scheme exists");
        let theme = aterm_render::Theme::from(&scheme);
        let ground = rgb(theme.bg);
        let row = RenderCell {
            ch: ' ',
            fg: rgb(theme.fg),
            bg: ground,
            ..RenderCell::default()
        };
        for mode in [Mode::Auto, Mode::Bypass] {
            let hue = mode.hue().expect("Claude gives it a hue");
            if contrast(hue, ground) < CHIP_INK_FLOOR {
                under.push((name, mode));
            }
            let mut w = lights(AutopilotView::Cruise, ChipGlass::default());
            w.sessions.get_mut(&7).expect("session 7").shown.mode = Some(mode);
            w.hover = Some((7, Target::from(Chip::Light(Light::Mode))));
            let block = w.block(7, row, now, ACCENT, Keep::Drawn);
            let pill: String = format!("{} {}", mode.glyph(), mode.word());
            let all = text(&block.lights);
            let at = all
                .find(&pill)
                .unwrap_or_else(|| panic!("{name} {mode:?}: the light drawn: {all:?}"));
            let first = all[..at].chars().count();
            let cells = &block.lights[first..first + pill.chars().count()];
            for cell in cells.iter().filter(|c| c.ch != ' ') {
                assert!(
                    contrast(cell.fg, ground) >= CHIP_INK_FLOOR,
                    "{name} {mode:?}: {:?} on {ground:?}",
                    cell.fg
                );
                if contrast(hue, ground) >= CHIP_INK_FLOOR {
                    assert_eq!(cell.fg, hue, "{name} {mode:?}: the hue itself");
                }
            }
        }
    }
    for mode in [Mode::Auto, Mode::Bypass] {
        assert!(
            under.contains(&("Aterm Day", mode)),
            "NEGATIVE CONTROL: the fixed {mode:?} hue fails a light ground: {under:?}"
        );
    }
}

/// WHAT THE TITLE SAYS BESIDE THE GLASS, ON EVERY FACE
/// (`docs/DESIGN-harness-autopilot-2026-10-15.md` §12.2.1, §12.3.1; the W4
/// title slice): every face of the table, with no hold and no need, with a
/// harness's need, with Land's hold read (`not landed yet: …`) and unread
/// (`landing since …`), and with a hold and a need together. With neither
/// the title is the table's own. A need follows the explanation, before the
/// clicks — ` · needs you: sign 0.48.0, the release credential, on this Mac
/// (+1 more); carrying on`, the worker's ` · ` and ` — ` made `, ` — on
/// every face a harness explains, none where no harness runs, and said once
/// under a read hold that names it. A hold is said under `⤓ land` alone:
/// read, it is the explanation (its legs, then `· landing since 14:02, 3
/// nudges`); unread, the sentence then its clock. Each title is sized
/// exactly, keeps one ` — ` at most and one ` · click: `, and its leaner
/// forms keep their cuts: the first (no trail) none, v1's words the whole
/// without the menu's line (the hold and the need kept), the clicks'
/// forms the phrase and the clicks alone (the hold and the need given up
/// with the explanation). The description says the hold, then the need,
/// glyph-free, and nothing where neither stands; the label is the face's
/// own whatever they say (WCAG 2.5.3: the painted word in it).
///
/// NEGATIVE CONTROLS (as run): `need_title` without `title_safe` gives the
/// need's part `sign 0.48.0 · the release credential — on this Mac`, and
/// the Cruise row's title fails its expected words and its one ` — `;
/// `TitleWords::need_under` without its face filter gives `⊘ off`'s
/// `harness off in Settings` title a need it cannot carry on.
#[test]
fn every_face_says_a_hold_and_a_need_where_its_harness_explains() {
    use crate::harness_glass::hold_tests::{ask, glass, held, unread};
    let need = ask();
    let (read, fresh) = (glass(held(), TODAY), glass(unread(), TODAY));
    let need_part = " \u{b7} needs you: sign 0.48.0, the release credential, on this Mac (+1 more); \
                     carrying on";
    let need_said = "Needs you: sign 0.48.0 the release credential on this Mac. And 1 more.";
    let legs = "2 unpushed commits; open work in its report: follow-up, not pushed";
    let since = "landing since 14:02, 3 nudges";
    let glyph_free = |text: &str| {
        text.chars()
            .all(|c| c.is_alphanumeric() || matches!(c, ' ' | '.' | ':' | ','))
    };
    for row in rows() {
        let face = row.face;
        let explained = !matches!(
            face,
            ChipFace::MasterOff | ChipFace::AgentOff(_) | ChipFace::Unhosted
        );
        let land = face == ChipFace::Land;
        let click_at = row.whole.rfind(" \u{b7} click: ").expect("a click");
        let (head, clicks) = row.whole.split_at(click_at);
        let menu_at = clicks.rfind(" \u{b7} right-click: more").expect("the menu");
        for (case, hold, said_need) in [
            ("none", None, None),
            ("a need", None, Some(&need)),
            ("a read hold", Some(&read), None),
            ("an unread hold", Some(&fresh), None),
            ("a read hold and a need", Some(&read), Some(&need)),
            ("an unread hold and a need", Some(&fresh), Some(&need)),
        ] {
            let words = TitleWords {
                hold,
                need: said_need,
            };
            let (whole, short) = autopilot_title(row.view, &row.glass, "", Some(TODAY), words);
            // What the table's title says beside the glass.
            let read_hold = land && hold.is_some_and(|h| h.read());
            let mut expected = match hold.filter(|_| land) {
                Some(h) if h.read() => {
                    let first = if said_need.is_some() {
                        "needs you: sign 0.48.0, the release credential, on this Mac (+1 more)"
                    } else {
                        "a need named earlier in its reply"
                    };
                    format!(
                        "{} \u{2014} not landed yet: {first}; {legs} \u{b7} {since}",
                        row.short
                    )
                }
                Some(_) => format!("{head} \u{b7} {since}"),
                None => head.to_owned(),
            };
            if said_need.is_some() && explained && !read_hold {
                expected.push_str(need_part);
            }
            let head_said = expected.clone();
            expected.push_str(clicks);
            assert_eq!(whole, expected, "{face:?}, {case}");
            assert_eq!(short, row.short, "{face:?}, {case}");
            assert_eq!(whole.capacity(), whole.len(), "sized exactly: {whole}");
            assert!(whole.matches(" \u{2014} ").count() <= 1, "{whole}");
            assert_eq!(whole.matches(" \u{b7} click: ").count(), 1, "{whole}");
            assert!(!whole.contains(" \u{b7} last: "), "{whole}");
            assert!(!whole.contains(" \u{b7} pausing until "), "{whole}");
            // The leaner forms keep their cuts.
            let [lean, v1, both, click] =
                lean_titles(&whole).map(|(head, tail)| format!("{head}{tail}"));
            assert_eq!(lean, "", "no trail: {whole}");
            assert_eq!(
                v1,
                format!("{head_said}{}", &clicks[..menu_at]),
                "v1's words keep the hold and the need: {whole}"
            );
            if whole.contains(" \u{2014} ") {
                assert_eq!(
                    click,
                    format!("{}{}", row.short, &clicks[..menu_at]),
                    "{whole}"
                );
                assert!(
                    both.is_empty() || both == format!("{}{clicks}", row.short),
                    "{whole}"
                );
            } else {
                assert_eq!((both.as_str(), click.as_str()), ("", ""), "{whole}");
            }
            for form in [&both, &click] {
                for word in ["needs you", "landing since", "not landed"] {
                    assert!(
                        !form.contains(word),
                        "given up with the explanation: {form}"
                    );
                }
            }
            // The description: the hold, then the need; the label the face's.
            let description = autopilot_description(face, &row.glass, words, Some(TODAY), None);
            let mut sentences = Vec::new();
            match hold.filter(|_| land) {
                Some(h) if h.read() => sentences.push(format!(
                    "Not landed yet. {} 2 unpushed commits. Open work in its report: follow up, \
                     not pushed. Landing since 14:02, 3 nudges.",
                    if said_need.is_some() {
                        need_said
                    } else {
                        "A need named earlier in its reply."
                    }
                )),
                Some(_) => sentences.push("Landing since 14:02, 3 nudges.".to_owned()),
                None => {}
            }
            if said_need.is_some() && explained && !read_hold {
                sentences.push(format!("{need_said} Carrying on."));
            }
            let wanted = (!sentences.is_empty()).then(|| sentences.join(" "));
            assert_eq!(description, wanted, "{face:?}, {case}");
            if let Some(description) = &description {
                assert!(glyph_free(description), "{description}");
            }
            assert_eq!(face.spoken(&row.glass, Some(TODAY)), row.spoken);
        }
    }
}

/// A NEED IN THE TITLE KEEPS ITS CUT MARKS (§12.2.3 item 6, slice 2e): a
/// worker's need holding ` · last: `, ` · click: ` and ` — ` gives a title
/// with the very words and the same four lean forms as the need already
/// written with `, ` — under a pause and a trail, so every cut stands — on
/// every face a harness explains. NEGATIVE CONTROL (as run): the need's raw
/// words (`need_title` without `title_safe`) cut the first leaner form at
/// the need's own ` · last: ` and the face's title fails the match.
#[test]
fn a_need_in_the_title_keeps_its_cut_marks() {
    use crate::harness_glass::PauseGlass;
    let raw = crate::needs::HarnessAsk {
        first: "sign 0.48.0 \u{b7} last: the release credential \u{2014} on this Mac \u{b7} \
                click: approve"
            .to_string(),
        more: 1,
        others: Vec::new(),
        program: aterm_phase::Program::Claude,
    };
    let written = crate::needs::HarnessAsk {
        first: "sign 0.48.0, last: the release credential, on this Mac, click: approve".to_string(),
        ..raw.clone()
    };
    let trail = "\u{2713} approved 14:05";
    for row in rows() {
        let glass = ChipGlass {
            pause: Some(PauseGlass::of(crate::PausePlan {
                at_unix: 1_792_159_500,
                short: 3,
            })),
            ..row.glass
        };
        let title = |need: &crate::needs::HarnessAsk| {
            autopilot_title(
                row.view,
                &glass,
                trail,
                Some(TODAY),
                TitleWords {
                    hold: None,
                    need: Some(need),
                },
            )
            .0
        };
        let (cut, plain) = (title(&raw), title(&written));
        assert_eq!(cut, plain, "{:?}", row.face);
        let forms = lean_titles(&cut);
        assert_eq!(forms, lean_titles(&plain), "{cut}");
        let [lean, v1, ..] = forms;
        assert!(!lean.0.is_empty(), "{cut}");
        assert!(!v1.0.is_empty(), "{cut}");
        if matches!(
            row.face,
            ChipFace::MasterOff | ChipFace::AgentOff(_) | ChipFace::Unhosted
        ) {
            assert!(!cut.contains("needs you"), "no harness carries on: {cut}");
        } else {
            assert!(
                lean.0.contains("; carrying on \u{b7} pausing until "),
                "the first leaner form keeps the need and the pause: {cut}"
            );
            assert!(v1.0.ends_with("; carrying on"), "v1 keeps the need: {cut}");
        }
    }
}

/// A HELD LANDING KEEPS THE TITLE'S FOUR LEAN FORMS (§12.3.1, slice 3d):
/// under `⤓ land` with a read hold whose need holds ` · ` and ` — `, a
/// pause and a trail, every leg's words hold no ` — `, ` · last: ` or ` ·
/// pausing until `; the four lean forms stand (without the trail; v1's
/// words, the hold kept; the clicks; the click), the last two with no hold;
/// and what a screen reader hears ([`App::autopilot_spoken`], the
/// accessibility node's and `chrome`'s words) is the face's label, unchanged
/// (`Autopilot: land.`, the painted word in it), and a description that
/// leads with the hold, glyph-free. NEGATIVE CONTROLS (as run): `speakable`
/// keeping a hyphen fails the glyph-free check (`follow-up`); the hold put
/// in the label fails the label's.
#[test]
fn a_held_landing_keeps_the_titles_four_lean_forms() {
    use crate::harness_glass::hold_tests::{ask, glass, held};
    let hold = glass(held(), TODAY);
    let need = ask();
    let pause = crate::harness_glass::PauseGlass::of(crate::PausePlan {
        at_unix: 1_792_159_500,
        short: 3,
    });
    let chip = ChipGlass {
        pause: Some(pause),
        ..ChipGlass::default()
    };
    let words = TitleWords {
        hold: Some(&hold),
        need: Some(&need),
    };
    let (whole, short) = autopilot_title(
        AutopilotView::Land,
        &chip,
        "\u{2913} nudged 14:22",
        Some(TODAY),
        words,
    );
    assert_eq!(short, "autopilot: land");
    let explanation = ChipFace::Land
        .explanation(&chip, words, Some(TODAY))
        .expect("the hold");
    let (legs, since) = explanation
        .split_once(" \u{b7} ")
        .expect("the legs, then the clock");
    assert_eq!(since, "landing since 14:02, 3 nudges", "{whole}");
    for leg in legs.split("; ") {
        for mark in [
            " \u{2014} ",
            " \u{b7} last: ",
            " \u{b7} pausing until ",
            " \u{b7} ",
        ] {
            assert!(!leg.contains(mark), "{leg:?} in {whole}");
        }
    }
    assert!(
        whole.starts_with(&format!(
            "autopilot: land \u{2014} {explanation} \u{b7} pausing until "
        )),
        "{whole}"
    );
    assert_eq!(whole.matches(" \u{2014} ").count(), 1, "{whole}");
    let [lean, v1, clicks, click] = lean_titles(&whole).map(|(head, tail)| format!("{head}{tail}"));
    assert_eq!(
        lean,
        whole.replace(" \u{b7} last: \u{2913} nudged 14:22", ""),
        "without the trail"
    );
    assert_eq!(
        v1,
        format!("autopilot: land \u{2014} {explanation} \u{b7} click: off"),
        "v1's words keep the hold"
    );
    assert_eq!(
        clicks,
        "autopilot: land \u{b7} click: off \u{b7} right-click: more"
    );
    assert_eq!(click, "autopilot: land \u{b7} click: off");
    // What a screen reader hears, through the App's one door.
    let mut app = App::headless_for_test();
    assert!(crate::harness_glass::on_land_held(
        &mut app,
        0,
        Some(held())
    ));
    let (label, description) = app.autopilot_spoken(0, AutopilotView::Land, &chip);
    assert_eq!(label, ChipFace::Land.spoken(&chip, Some(TODAY)));
    assert_eq!(label, "Autopilot: land.", "the painted word, unchanged");
    let description = description.expect("the hold described");
    assert!(
        description
            .starts_with("Not landed yet. A need named earlier in its reply. 2 unpushed commits."),
        "{description}"
    );
    assert!(
        description
            .chars()
            .all(|c| c.is_alphanumeric() || matches!(c, ' ' | '.' | ':' | ',')),
        "glyph-free: {description}"
    );
    assert!(description.contains(" Pausing until "), "{description}");
}
