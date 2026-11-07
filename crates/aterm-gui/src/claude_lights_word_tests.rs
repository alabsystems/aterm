// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! THE MODE'S NAME A CLICK BRINGS UP ([`ChipWord`]): a click on the Autopilot chip lays the mode
//! it reached after the glyph (`⤓ land`), in the chip's tone, holds it, fades it into the ground
//! a step at a time and lets it go, the glyph where it stood throughout; the name takes only
//! cells the rule spares.

use std::time::{Duration, Instant};

use aterm_agent::supervise::Autopilot;
use aterm_core::terminal::RenderCell;
use aterm_window::event::{ElementState, MouseButton};

use super::gesture_tests::{
    BYPASS, app, cell_at, draw_claude, feed, frame, host, publish_facts, px_of, row_text,
};
use super::{Chip, ChipAt, ChipWord, Inks, WORD_FADE, WORD_HOLD, WORD_STEPS, WindowLights};
use crate::autopilot::AutopilotView;
use crate::{App, WindowId};

const S: u64 = 7;

fn blank() -> RenderCell {
    RenderCell {
        ch: ' ',
        fg: [200, 200, 200],
        bg: [0, 0, 0],
        ..RenderCell::default()
    }
}

fn ms(n: u64) -> Duration {
    Duration::from_millis(n)
}

/// Land's name, brought up at `from`.
const fn land_at(from: Instant, fades: bool) -> ChipWord {
    ChipWord {
        mode: Autopilot::Land,
        from,
        fades,
        asked: 0,
    }
}

/// A block's cells as text, a wide character's tail left out.
fn text(cells: &[RenderCell]) -> String {
    cells.iter().filter(|c| !c.wide).map(|c| c.ch).collect()
}

/// How far `ink` stands from `ground`, channel by channel.
fn far(ink: [u8; 3], ground: [u8; 3]) -> u32 {
    ink.iter()
        .zip(ground)
        .map(|(a, b)| u32::from(a.abs_diff(b)))
        .sum()
}

/// A NAME HOLDS, THEN FADES A STEP AT A TIME AND GOES: whole for [`WORD_HOLD`], then each of
/// [`WORD_STEPS`] steps nearer the ground and never on it, gone when [`WORD_FADE`] ends; every
/// step lasts until the instant `next_at` names and changes there, so the loop's wake is each
/// frame the name needs and no other. Under Reduce Motion it holds as long and goes at once.
#[test]
fn a_clicks_mode_name_holds_then_fades_a_step_at_a_time_and_goes() {
    let t0 = Instant::now();
    let named = land_at(t0, true);
    assert_eq!(named.step(t0), Some(0), "whole at first");
    assert_eq!(
        named.step(t0 + WORD_HOLD - ms(1)),
        Some(0),
        "whole as it holds"
    );
    let mut at = t0;
    let mut steps = Vec::new();
    while let Some(next) = named.next_at(at) {
        assert!(next > at, "a wake ahead of {at:?}");
        let step = named.step(at).expect("on show");
        steps.push(step);
        assert_eq!(
            named.step(next - Duration::from_nanos(1)),
            Some(step),
            "step {step} lasts until its wake"
        );
        assert_ne!(named.step(next), Some(step), "and changes there");
        at = next;
    }
    assert_eq!(
        steps,
        (0..=WORD_STEPS).collect::<Vec<_>>(),
        "every step, in order"
    );
    assert_eq!(at, t0 + WORD_HOLD + WORD_FADE, "gone when the fade ends");
    assert_eq!(named.step(at), None);

    let ground = blank().bg;
    let letter = Inks::of(blank(), ChipAt::cruise(blank())).letter;
    assert_eq!(
        ChipWord::ink(0, letter, ground),
        letter,
        "whole: the chip's own tone"
    );
    for step in 1..=WORD_STEPS {
        let (was, now) = (
            ChipWord::ink(step - 1, letter, ground),
            ChipWord::ink(step, letter, ground),
        );
        assert!(
            far(now, ground) < far(was, ground),
            "step {step} nearer the ground"
        );
        assert_ne!(now, ground, "step {step} still drawn");
    }

    let still = land_at(t0, false);
    assert_eq!(
        still.step(t0 + WORD_HOLD - ms(1)),
        Some(0),
        "Reduce Motion: it holds"
    );
    assert_eq!(
        still.next_at(t0),
        Some(t0 + WORD_HOLD),
        "one wake: its going"
    );
    assert_eq!(still.step(t0 + WORD_HOLD), None, "and goes at once");
}

/// THE NAME FOLLOWS THE GLYPH, IS THE CHIP'S, AND GIVES WAY FIRST: Land at rest is `⤓` alone;
/// with its name, `⤓ land` in the chip's letter, the whole of it the chip's (a click on the name
/// cycles too), set off from the next chip as any word is (`⤓ land ─ ⏸ plan`),
/// [`super::Block::word`] counting what it adds; a rule with room for the rest alone lays the rest, never a narrower
/// rung; a name for another mode, or a click in flight, lays none.
#[test]
fn the_name_follows_the_glyph_in_its_tone_where_the_rest_leaves_room() {
    let now = Instant::now();
    let mut w = WindowLights::default();
    w.note_autopilot(S, Some(Autopilot::Land), AutopilotView::Land, now);
    let rest = w
        .fitted(S, blank(), now, ChipAt::cruise(blank()), None, None, 40)
        .expect("fits");
    assert_eq!(
        text(&rest.lights),
        "\u{2913}",
        "Land at rest: the glyph alone"
    );
    assert_eq!(rest.word, 0);

    w.sessions.get_mut(&S).expect("noted").word = Some(land_at(now, true));
    let named = w
        .fitted(S, blank(), now, ChipAt::cruise(blank()), None, None, 40)
        .expect("fits");
    assert_eq!(text(&named.lights), "\u{2913} land");
    assert_eq!(named.word, " land".len(), "the name's cells");
    assert_eq!(named.holds, rest.holds + named.word);
    assert_eq!(
        named.cols,
        [(0..6, Chip::Autopilot)],
        "the name is the chip's"
    );
    let letter = ChipAt::cruise(blank()).letter_at(0);
    assert!(
        named.lights.iter().all(|c| c.ch == ' ' || c.fg == letter),
        "the chip's letter"
    );

    w.sessions.get_mut(&S).expect("noted").shown = lights_plan();
    let word = w.sessions.get_mut(&S).expect("noted").word.take();
    let rest_plan = w
        .fitted(S, blank(), now, ChipAt::cruise(blank()), None, None, 40)
        .expect("fits");
    assert_eq!(text(&rest_plan.lights), "\u{2913} \u{23F8} plan");
    w.sessions.get_mut(&S).expect("noted").word = word;
    let beside = w
        .fitted(S, blank(), now, ChipAt::cruise(blank()), None, None, 40)
        .expect("fits");
    assert_eq!(
        text(&beside.lights),
        "\u{2913} land \u{2500} \u{23F8} plan",
        "set off as a word is"
    );
    assert_eq!(
        beside.rule_gaps().collect::<Vec<_>>(),
        [7],
        "the rule's own `─`"
    );
    assert_eq!(
        beside.word,
        " land \u{2500}".chars().count(),
        "the name and its `─`"
    );
    assert_eq!(beside.holds, rest_plan.holds + beside.word);

    let tight = w
        .fitted(
            S,
            blank(),
            now,
            ChipAt::cruise(blank()),
            None,
            None,
            rest_plan.holds,
        )
        .expect("the rest fits");
    assert_eq!(
        tight.lights, rest_plan.lights,
        "room for the rest alone: the rest"
    );
    assert_eq!(tight.word, 0);
    let short = w
        .fitted(
            S,
            blank(),
            now,
            ChipAt::cruise(blank()),
            None,
            None,
            beside.holds - 1,
        )
        .expect("the rest fits");
    assert_eq!(short.word, 0, "a cell short of the name: none");
    let room = w
        .fitted(
            S,
            blank(),
            now,
            ChipAt::cruise(blank()),
            None,
            None,
            beside.holds,
        )
        .expect("fits");
    assert_eq!(room.word, beside.word, "CONTROL: room for it, the name");

    w.note_autopilot(S, Some(Autopilot::Off), AutopilotView::Off, now);
    let other = w
        .fitted(S, blank(), now, ChipAt::cruise(blank()), None, None, 40)
        .expect("fits");
    assert!(
        text(&other.lights).starts_with("\u{25cb} \u{23F8}"),
        "Land's name never names Off"
    );
    assert_eq!(other.word, 0);

    w.note_autopilot(S, Some(Autopilot::Land), AutopilotView::Land, now);
    w.sessions.get_mut(&S).expect("noted").autopilot_pending =
        Some((Autopilot::Off, now + ms(2500)));
    let flight = w
        .fitted(S, blank(), now, ChipAt::cruise(blank()), None, None, 40)
        .expect("fits");
    assert_eq!(flight.word, 0, "a click in flight: `◌`, no name");
}

/// Claude in plan mode: a light drawn beside Autopilot.
fn lights_plan() -> aterm_agent::harness::lights::Reading {
    aterm_agent::harness::lights::Reading {
        mode: Some(aterm_agent::harness::lights::Mode::Plan),
        pill: true,
    }
}

/// A NAME STANDS FROM ITS READ-BACK, AND A CLICK THAT NEVER READS BACK SHOWS NONE: a click whose
/// word is still being written shows `◌` and no name, and its name waits (no frame owed, no
/// expiry) however long the write takes; the frame that reads the word back starts the name's
/// hold there; a write that times out lets the name go with the `◌`.
#[test]
fn a_name_stands_from_its_read_back_and_goes_with_a_click_that_times_out() {
    let t0 = Instant::now();
    let mut w = WindowLights::default();
    w.note_autopilot(S, Some(Autopilot::Cruise), AutopilotView::Cruise, t0);
    {
        let s = w.sessions.get_mut(&S).expect("noted");
        s.word = Some(land_at(t0, true));
        s.autopilot_pending = Some((Autopilot::Land, t0 + ms(2500)));
    }
    // Past the name's hold and fade from the click, the write still in flight.
    let read = t0 + ms(2000);
    w.note_autopilot(S, Some(Autopilot::Cruise), AutopilotView::Cruise, read);
    assert!(
        w.sessions[&S].autopilot_pending.is_some(),
        "not read back yet"
    );
    assert!(!w.words_due(read), "no frame owed for a name in flight");
    let _ = w.expire(read);
    assert!(
        w.sessions[&S].word.is_some(),
        "the name waits for its read-back"
    );
    w.note_autopilot(S, Some(Autopilot::Land), AutopilotView::Land, read);
    let s = &w.sessions[&S];
    assert!(s.autopilot_pending.is_none(), "read back");
    assert_eq!(
        s.word.map(|n| n.from),
        Some(read),
        "the name stands from the read-back"
    );
    assert_eq!(
        s.word_at(read + WORD_HOLD - ms(1)),
        Some(("land", 0)),
        "a whole hold"
    );

    let mut w = WindowLights::default();
    w.note_autopilot(S, Some(Autopilot::Cruise), AutopilotView::Cruise, t0);
    {
        let s = w.sessions.get_mut(&S).expect("noted");
        s.word = Some(land_at(t0, false));
        s.autopilot_pending = Some((Autopilot::Land, t0 + ms(2500)));
    }
    let read = t0 + ms(1200);
    assert!(
        !w.words_due(read),
        "Reduce Motion: no frame owed in flight either"
    );
    let _ = w.expire(read);
    w.note_autopilot(S, Some(Autopilot::Land), AutopilotView::Land, read);
    assert_eq!(
        w.sessions[&S].word_at(read + WORD_HOLD - ms(1)),
        Some(("land", 0)),
        "Reduce Motion: a whole hold from the read-back"
    );

    let mut w = WindowLights::default();
    w.note_autopilot(S, Some(Autopilot::Cruise), AutopilotView::Cruise, t0);
    {
        let s = w.sessions.get_mut(&S).expect("noted");
        s.word = Some(land_at(t0, true));
        s.autopilot_pending = Some((Autopilot::Land, t0 + ms(2500)));
    }
    w.note_autopilot(
        S,
        Some(Autopilot::Cruise),
        AutopilotView::Cruise,
        t0 + ms(2500),
    );
    let s = &w.sessions[&S];
    assert!(s.autopilot_pending.is_none(), "timed out");
    assert_eq!(s.word, None, "and its name with it");
}

/// THE LOOP WAKES FOR EACH STEP AND NO MORE: `word_wake` names the next step's instant; at it,
/// `words_due` asks the window for one frame, then none until the next step; the name gone, it
/// asks one last frame (to clear it) and the wake is none.
#[test]
fn the_loop_wakes_for_each_step_and_asks_one_frame_a_step() {
    let t0 = Instant::now();
    let mut w = WindowLights::default();
    w.note_autopilot(S, Some(Autopilot::Land), AutopilotView::Land, t0);
    w.sessions.get_mut(&S).expect("noted").word = Some(land_at(t0, true));
    assert!(!w.words_due(t0), "the click's own frame drew step 0");
    let mut at = t0;
    let mut frames = 0;
    while let Some(next) = w.word_wake(at) {
        assert!(
            !w.words_due(next - Duration::from_nanos(1)),
            "nothing owed before the wake"
        );
        assert!(w.words_due(next), "a frame at the wake");
        assert!(!w.words_due(next), "one frame a step");
        frames += 1;
        at = next;
    }
    assert_eq!(
        frames,
        WORD_STEPS + 1,
        "one frame a step, and one to clear it"
    );
    assert_eq!(w.sessions[&S].word, None, "let go");
    assert_eq!(w.word_wake(at), None, "no wake after");
}

/// A NAME THAT ENDS BETWEEN TWO LOOKS STILL GETS ITS LAST FRAME (review chipword-wake-race): a
/// look just before the name's end owes no frame (its last step is drawn), and the loop's wake,
/// asked once the end has passed and the name not let go yet, is due at once, so the next look
/// lets it go and asks the frame that clears it; then no wake. Whole under Reduce Motion, faded
/// where motion is allowed.
#[test]
fn a_name_that_ends_between_two_looks_still_wakes_the_loop() {
    let t0 = Instant::now();
    for fades in [true, false] {
        let mut w = WindowLights::default();
        w.note_autopilot(S, Some(Autopilot::Land), AutopilotView::Land, t0);
        let last = if fades { WORD_STEPS } else { 0 };
        let end = t0 + WORD_HOLD + if fades { WORD_FADE } else { Duration::ZERO };
        w.sessions.get_mut(&S).expect("noted").word = Some(ChipWord {
            asked: last,
            ..land_at(t0, fades)
        });
        assert!(
            !w.words_due(end - Duration::from_nanos(1)),
            "fades={fades}: its last step is drawn, no frame owed"
        );
        let wake = w.word_wake(end);
        assert!(
            wake.is_some_and(|at| at <= end),
            "fades={fades}: gone but not let go, a wake due at once: {wake:?}"
        );
        assert!(
            w.words_due(end),
            "fades={fades}: the next look owes the frame that clears it"
        );
        assert_eq!(w.sessions[&S].word, None, "fades={fades}: let go");
        assert_eq!(w.word_wake(end), None, "fades={fades}: no wake after");
    }
}

/// The frame cell of the Autopilot chip's glyph: the chip's first painted cell.
fn glyph(app: &App, wid: WindowId) -> (usize, usize) {
    app.windows[&wid]
        .claude_lights
        .hits
        .iter()
        .filter(|h| h.chip == Some(Chip::Autopilot))
        .map(|h| (h.frame_row, h.col))
        .min_by_key(|(_, col)| *col)
        .expect("the chip is painted")
}

fn click(app: &mut App, wid: WindowId, cell: (usize, usize)) {
    let (x, y) = px_of(app, wid, cell);
    app.on_cursor_moved(wid, x, y);
    app.on_mouse_input(wid, ElementState::Pressed, MouseButton::Left);
    app.on_mouse_input(wid, ElementState::Released, MouseButton::Left);
}

/// When `session`'s name in `wid` came up.
fn word_from(app: &App, wid: WindowId, session: u64) -> Instant {
    app.windows[&wid].claude_lights.sessions[&session]
        .word
        .expect("a name on show")
        .from
}

/// A frame of `wid` with the lights' clock at `now`.
fn frame_at(app: &mut App, wid: WindowId, now: Instant) {
    super::at_clock(now, || frame(app, wid));
}

/// Each chip's painted cells but the Autopilot chip's, in frame columns.
fn others(app: &App, wid: WindowId) -> Vec<(Option<Chip>, usize)> {
    app.windows[&wid]
        .claude_lights
        .hits
        .iter()
        .filter(|h| h.chip != Some(Chip::Autopilot))
        .map(|h| (h.chip, h.col))
        .collect()
}

/// The facts' columns the frame laid.
fn facts(app: &App, wid: WindowId) -> Vec<(usize, usize)> {
    app.windows[&wid]
        .claude_lights
        .fact_hits
        .iter()
        .map(|h| h.cols)
        .collect()
}

/// CLICKING THROUGH THE MODES NAMES EACH, THE GLYPH STAYING UNDER THE POINTER: each click on the
/// glyph brings up the mode it reached (`⤓ land`, `○ off`, `∞ cruise`) in that mode's tone, the
/// glyph where it stood so the next click lands on it again; held, the name fades and goes, the
/// glyph alone where it stood. Each frame reads the lights' clock where the test pins it.
#[test]
fn clicking_through_the_modes_names_each_with_the_glyph_under_the_pointer() {
    let (mut app, wid, session, _reader) = app();
    let _host = host(&mut app);
    assert!(app.apply_term_resize(wid, 24, 120));
    publish_facts(&app, session);
    draw_claude(&app, session, BYPASS);
    feed(&app, session, b"\x1b[?1000h\x1b[?1006h");
    frame(&mut app, wid);
    let at = glyph(&app, wid);
    assert_eq!(cell_at(&app, wid, at).ch, '\u{221e}', "Cruise at rest");
    // The name's first letter, a blank after the glyph.
    let name_at = (at.0, at.1 + 2);
    let mut from = Instant::now();
    for (mode, name, mark) in [
        (Autopilot::Land, "land", '\u{2913}'),
        (Autopilot::Off, "off", '\u{25cb}'),
        (Autopilot::Cruise, "cruise", '\u{221e}'),
    ] {
        click(&mut app, wid, at);
        assert_eq!(
            app.session_autopilot(session).0,
            Some(mode),
            "the click stepped"
        );
        from = word_from(&app, wid, session);
        frame_at(&mut app, wid, from);
        assert_eq!(
            glyph(&app, wid),
            at,
            "{name}: the glyph stays under the pointer"
        );
        assert_eq!(cell_at(&app, wid, at).ch, mark, "{name}: its glyph");
        let row = row_text(&app, wid, at.0);
        assert!(
            row.contains(&format!("{mark} {name}")),
            "{name} follows: {row:?}"
        );
        assert_eq!(
            cell_at(&app, wid, name_at).ch,
            name.chars().next().unwrap_or(' ')
        );
        assert!(
            app.windows[&wid]
                .claude_lights
                .hits
                .iter()
                .any(|h| h.chip == Some(Chip::Autopilot) && (h.frame_row, h.col) == name_at),
            "{name}: the chip's own cells"
        );
        assert_eq!(
            cell_at(&app, wid, name_at).fg,
            cell_at(&app, wid, at).fg,
            "{name}: whole, in the glyph's tone"
        );
    }
    let ground = cell_at(&app, wid, at).bg;
    let whole = cell_at(&app, wid, name_at).fg;
    // The middle of the fade's fourth step.
    let fading_at = from + WORD_HOLD + WORD_FADE / 2 + WORD_FADE / (2 * WORD_STEPS);
    assert!(
        app.claude_chip_words_due(fading_at).contains(&wid),
        "a frame owed"
    );
    frame_at(&mut app, wid, fading_at);
    assert_eq!(cell_at(&app, wid, name_at).ch, 'c', "still laid");
    let fading = cell_at(&app, wid, name_at).fg;
    assert!(
        far(fading, ground) < far(whole, ground),
        "fading toward the ground"
    );
    assert_ne!(fading, ground, "still drawn");
    let gone = from + WORD_HOLD + WORD_FADE;
    assert!(
        app.claude_chip_words_due(gone).contains(&wid),
        "its going owed"
    );
    frame_at(&mut app, wid, gone);
    assert_eq!(
        app.windows[&wid]
            .claude_lights
            .hits
            .iter()
            .filter(|h| h.chip == Some(Chip::Autopilot))
            .map(|h| (h.frame_row, h.col))
            .collect::<Vec<_>>(),
        [at],
        "the glyph alone where it stood"
    );
    assert!(
        !row_text(&app, wid, at.0).contains("\u{221e} cruise"),
        "the name gone"
    );
    assert_eq!(app.claude_chip_word_wake(gone), None, "and no wake");
}

/// THE NAME TAKES ONLY CELLS THE RULE SPARES: across widths, a frame with the name lays every
/// cell up to the glyph, every fact and its cells, and a title wherever the frame without it lays
/// one, as that frame does; the name (` land`, with the ` ─` that sets it off from a chip after
/// it) after the glyph, every chip after it moved on by the name's cells, over the rule's own
/// `─` or a title cut to a narrower form (the title gives way first); where the rest leaves too
/// few, the frame is the rest's whole. Both happen across the sweep (a control for each).
#[test]
fn the_name_takes_only_cells_the_rule_spares() {
    let (mut laid, mut withheld) = (0, 0);
    for cols in (30..=96).step_by(3) {
        let (mut app, wid, session, _reader) = app();
        let _host = host(&mut app);
        assert!(app.apply_term_resize(wid, 24, cols));
        publish_facts(&app, session);
        draw_claude(&app, session, BYPASS);
        feed(&app, session, b"\x1b[?1000h\x1b[?1006h");
        frame(&mut app, wid);
        if !super::gesture_tests::painted(&app, wid, Chip::Autopilot) {
            continue;
        }
        let at = glyph(&app, wid);
        click(&mut app, wid, at);
        let t = word_from(&app, wid, session) + WORD_HOLD + WORD_FADE;
        assert!(
            app.claude_chip_words_due(t).contains(&wid),
            "{cols}: the click's name went"
        );
        frame_at(&mut app, wid, t);
        let rest = app.windows[&wid].input_scratch.cells[at.0].clone();
        let (rest_others, rest_facts) = (others(&app, wid), facts(&app, wid));
        let titled = !app.windows[&wid].claude_lights.title_hits.is_empty();
        let at = glyph(&app, wid);
        app.windows
            .get_mut(&wid)
            .and_then(|ws| ws.claude_lights.sessions.get_mut(&session))
            .expect("noted")
            .word = Some(land_at(t, true));
        frame_at(&mut app, wid, t);
        let named = app.windows[&wid].input_scratch.cells[at.0].clone();
        if named == rest {
            withheld += 1;
            continue;
        }
        laid += 1;
        assert_eq!(
            named[..=at.1],
            rest[..=at.1],
            "{cols}: up to the glyph, as the rest"
        );
        let after = at.1 + 1;
        let word = if rest_others.is_empty() {
            " land".len()
        } else {
            " land \u{2500}".chars().count()
        };
        let laid_name = text(&named[after..after + word]);
        assert_eq!(
            laid_name.trim_end_matches(['\u{2500}', ' ']),
            " land",
            "{cols}"
        );
        assert_eq!(
            others(&app, wid),
            rest_others
                .iter()
                .map(|&(chip, col)| (chip, col + word))
                .collect::<Vec<_>>(),
            "{cols}: every chip after it moved on by the name's cells"
        );
        assert!(
            !rest_facts.is_empty(),
            "{cols}: CONTROL: the rule lays facts"
        );
        assert_eq!(
            facts(&app, wid),
            rest_facts,
            "{cols}: every fact where it stood"
        );
        let first = rest_facts.iter().map(|f| f.0).min().unwrap_or(0);
        assert_eq!(named[first..], rest[first..], "{cols}: and its cells");
        assert_eq!(
            !app.windows[&wid].claude_lights.title_hits.is_empty(),
            titled,
            "{cols}: a title where the rest lays one"
        );
    }
    assert!(laid > 0, "CONTROL: some width lays the name");
    assert!(withheld > 0, "CONTROL: some width spares it no cells");
}

/// A NAME `word_at` DOES NOT DRAW READS NO CLOCK AND MOVES NO KEY, AND AN OFF SWITCH LETS IT GO:
/// while a click is in flight, the mode has moved on, or a standing task face (`Landed`) owns the
/// chip, neither `fingerprint` nor `block_key` reads the clock and `fingerprint` is 0; when the
/// harness switch turns off, `note_switch_face` clears `word` so `word_wake` and `words_due` stop
/// immediately.
#[test]
fn a_suppressed_or_switched_off_mode_name_reads_no_clock_and_moves_no_key() {
    use crate::autopilot::{HarnessSwitch, SwitchOff};
    use crate::claude_lights::Clock;
    use crate::harness_glass::Stamp;
    use aterm_agent::harness::acts::TaskFace;
    use std::cell::Cell;

    let t0 = Instant::now();
    let mut w = WindowLights::default();
    w.note_autopilot(S, Some(Autopilot::Land), AutopilotView::Land, t0);
    w.sessions.get_mut(&S).expect("noted").word = Some(land_at(t0, true));

    let reads = Cell::new(0);
    assert_ne!(
        w.fingerprint(|| {
            reads.set(reads.get() + 1);
            t0
        }),
        0,
        "CONTROL: a drawn mode name moves the repaint key"
    );
    assert_eq!(reads.get(), 1, "CONTROL: fingerprint read the clock once");
    let key = w.sessions[&S].block_key(|| {
        reads.set(reads.get() + 1);
        t0
    });
    assert_eq!(key.word, Some(("land", 0)));
    assert_eq!(reads.get(), 2, "CONTROL: block_key read the clock once");

    // 1. Click in flight (`autopilot_pending.is_some()`).
    w.sessions.get_mut(&S).expect("noted").autopilot_pending =
        Some((Autopilot::Off, t0 + ms(2500)));
    assert_eq!(
        w.fingerprint(|| panic!("in-flight click read the clock in fingerprint")),
        0
    );
    assert_eq!(
        w.sessions[&S]
            .block_key(|| panic!("in-flight click read the clock in block_key"))
            .word,
        None
    );
    w.sessions.get_mut(&S).expect("noted").autopilot_pending = None;

    // 2. Mode moved to another word (`AutopilotView::Off`).
    w.note_autopilot(S, Some(Autopilot::Off), AutopilotView::Off, t0);
    assert_eq!(
        w.fingerprint(|| panic!("other mode read the clock in fingerprint")),
        0
    );
    assert_eq!(
        w.sessions[&S]
            .block_key(|| panic!("other mode read the clock in block_key"))
            .word,
        None
    );
    assert_eq!(w.word_wake(t0), None, "other mode schedules no wake");
    assert!(
        !w.words_due(t0 + WORD_HOLD),
        "other mode owes no frame at hold end"
    );

    // 3. Standing task face (`TaskFace::Landed`) under `AutopilotView::Land`.
    w.note_autopilot(S, Some(Autopilot::Land), AutopilotView::Land, t0);
    {
        let glass = &mut w.sessions.get_mut(&S).expect("noted").glass;
        glass.task = Some(TaskFace::Landed);
        glass.task_at = Some(Stamp {
            clock: Clock::of_minute(17 * 60 + 42),
            day: 20_378,
        });
    }
    assert_eq!(
        w.fingerprint(|| panic!("standing face read the clock in fingerprint")),
        0
    );
    assert_eq!(
        w.sessions[&S]
            .block_key(|| panic!("standing face read the clock in block_key"))
            .word,
        None
    );
    assert_eq!(w.word_wake(t0), None, "standing face schedules no wake");
    assert!(
        !w.words_due(t0 + WORD_HOLD),
        "standing face owes no frame at hold end"
    );
    assert!(
        !w.words_due(t0 + WORD_HOLD + WORD_FADE),
        "standing face owes no frame when suppressed word expires"
    );
    assert_eq!(
        w.sessions[&S].word, None,
        "expired suppressed word is still dropped"
    );
    w.sessions.get_mut(&S).expect("noted").word = Some(land_at(t0, true));

    // 4. Turning the harness switch off clears `word` and cancels its wake.
    w.sessions.get_mut(&S).expect("noted").glass = super::ChipGlass::default();
    assert!(w.word_wake(t0).is_some(), "CONTROL: wake armed while on");
    w.note_switch_face(S, Some(HarnessSwitch::Off(SwitchOff::Master)));
    assert_eq!(w.sessions[&S].word, None, "off switch clears the mode name");
    assert_eq!(w.word_wake(t0), None, "no wake after switch turns off");
    assert!(
        !w.words_due(t0 + WORD_HOLD),
        "no frame owed after switch turns off"
    );
}
