// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! THE ACT TICKER ON A REAL FRAME (`docs/DESIGN-harness-autopilot-2026-10-15.md`
//! §9.2.5): a headless App with a harness host, Claude Code's bottom block
//! on its engine, the footer spliced as the window splices it — the label
//! lit by an act told as the host tells it, beside the whole facts or not at
//! all, for its dwell; withheld by `[claude_footer] acts = false` and by
//! serious mode; the trail in a hovered chip's title, built once per
//! reading; and what each of these costs a frame.

use std::os::unix::net::UnixStream;
use std::time::{Duration, Instant};

use aterm_agent::harness::acts::Act;

use super::Chip;
use super::gesture_tests::{
    BYPASS, Hosted, WHOLE_FACTS, app, cell_at, draw_claude, footer_fp, footer_row, frame, hit,
    host, painted, publish_facts, publish_facts_at, row_text,
};
use crate::{App, WindowId};

use crate::harness_glass::{Arrival, on_act, on_act_at};

/// A switch a test flips (`true`: the label withheld), by name.
type Switch = (&'static str, fn(&mut App, bool));

/// The chips at rest in a hosted Cruise session: Autopilot's `∞` and `⚙`.
const CHIPS: &str = "\u{221e} \u{2699}";

/// `app` hosted, its session drawing Claude Code in bypass under the
/// owner's whole facts, `cols` wide, one frame painted.
fn ticker_app(cols: u16) -> (App, WindowId, u64, (Hosted, UnixStream)) {
    let (mut app, wid, session, reader) = app();
    let hosted = host(&mut app);
    assert!(app.apply_term_resize(wid, 24, cols));
    publish_facts(&app, session);
    draw_claude(&app, session, BYPASS);
    frame(&mut app, wid);
    (app, wid, session, (hosted, reader))
}

/// The footer rule as it reads on the glass.
fn rule(app: &App, wid: WindowId) -> String {
    row_text(app, wid, footer_row(app, wid))
}

/// An act's arrival `ms` after `t0`, read in UTC at 14:05.
fn arrival(t0: Instant, ms: u64) -> Arrival {
    Arrival {
        now: t0 + Duration::from_millis(ms),
        unix: 1_792_159_500,
        offset_s: Some(0),
    }
}

/// An act's arrival `ms` after `t0` on the wall clock as it reads now, in
/// the zone's time — for a title built at the wall's now, whose trail
/// leaves out an act older than a day — and the clock text it carries.
fn arrival_now(t0: Instant, ms: u64) -> (Arrival, String) {
    let unix = i64::try_from(
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .expect("after the epoch")
            .as_secs(),
    )
    .expect("a second");
    let offset_s = crate::presence::local_offset_at(unix);
    let minute = (unix + offset_s.unwrap_or(0)).rem_euclid(86_400) / 60;
    let clock = crate::claude_lights::Clock::of_minute(u16::try_from(minute).expect("a minute"));
    (
        Arrival {
            now: t0 + Duration::from_millis(ms),
            unix,
            offset_s,
        },
        clock.as_str().to_owned(),
    )
}

/// Clear the counters the ticker's costs are read from.
fn reset_counts() {
    let _ = (
        crate::work_counts::take_footer_rules_composed(),
        crate::work_counts::take_harness_clock_reads(),
    );
}

/// AN ACT'S LABEL LIGHTS BESIDE THE WHOLE FACTS FOR ITS DWELL, THEN GOES
/// (§9.2.1): told as the host tells it, `✓ approved` stands right of the
/// chips in Cruise's ink, the chips and the facts where they were; frame
/// after frame during the dwell it costs no clock read beyond the act's
/// receipt and no composition beyond the label's own; at its end the rule is
/// the rest rule again, the repaint key its rest value, nothing armed and
/// the lights' key 0 — two compositions for the whole label.
///
/// NEGATIVE CONTROL: the key moved while it was lit (the label is drawn by
/// the footer's own key, never by a timer of its own).
#[test]
fn an_acts_label_lights_beside_the_whole_facts_for_its_dwell_then_goes() {
    let (mut app, wid, session, _host) = ticker_app(120);
    let rest = rule(&app, wid);
    assert!(
        rest.contains(CHIPS) && rest.contains(WHOLE_FACTS),
        "{rest:?}"
    );
    let rest_key = footer_fp(&app, wid);
    reset_counts();
    on_act(&mut app, session, Act::Approved);
    let lit_key = footer_fp(&app, wid);
    assert_ne!(lit_key, rest_key, "CONTROL: the key moves with the label");
    for i in 0..10 {
        frame(&mut app, wid);
        let lit = rule(&app, wid);
        assert!(
            lit.contains(&format!("{CHIPS}  \u{2713} approved")),
            "frame {i}: {lit:?}"
        );
        assert!(lit.contains(WHOLE_FACTS), "frame {i}: the facts whole");
        assert_eq!(footer_fp(&app, wid), lit_key, "frame {i}");
    }
    let (row, col) = hit(&app, wid, Chip::Autopilot);
    // `∞ ⚙`, the title's two-cell gap, then the label.
    let check = cell_at(&app, wid, (row, col + 5));
    assert_eq!(check.ch, '\u{2713}');
    assert_eq!(
        check.fg,
        crate::chrome_band::band_colors(app.theme).accent,
        "Cruise's ink"
    );
    assert_eq!(
        (
            crate::work_counts::take_harness_clock_reads(),
            crate::work_counts::take_footer_rules_composed()
        ),
        (1, 1),
        "the act's receipt and the label's composition, and nothing per frame"
    );
    let end = app.harness_acts_deadline().expect("the label's end");
    app.expire_harness_acts_at(end);
    for _ in 0..3 {
        frame(&mut app, wid);
    }
    assert_eq!(rule(&app, wid), rest, "the rest rule again");
    assert_eq!(footer_fp(&app, wid), rest_key, "the rest key again");
    assert_eq!(app.harness_acts_deadline(), None, "nothing armed");
    assert_eq!(app.windows[&wid].claude_lights.fingerprint(Instant::now), 0);
    assert_eq!(
        crate::work_counts::take_footer_rules_composed(),
        1,
        "its going"
    );
}

/// THE LABEL NEVER TAKES A FACT'S ROOM (§9.2.1): walked from a wide pane
/// down to a narrow one with a label lit, over the owner's typical facts
/// (whose rungs differ by less than the label), the rule draws it only
/// beside the WHOLE facts — the label never cuts the path or drops the
/// branch to make room — and the chips never move for it. At the width it
/// first goes, the facts are still whole: it gave way first. Where the
/// facts fell for want of room before any label, see
/// [`a_split_panes_cut_path_leaves_the_label_and_the_hovered_title_room`].
/// NEGATIVE CONTROL: it is drawn somewhere.
#[test]
fn the_acts_label_is_drawn_beside_the_whole_facts_or_not_at_all() {
    let t0 = Instant::now();
    let (mut app, wid, session, _host) = ticker_app(120);
    on_act_at(&mut app, session, Act::Approved, arrival(t0, 0));
    let at_left = hit(&app, wid, Chip::Autopilot).1;
    let mut drawn = Vec::new();
    for cols in (40..=120).rev() {
        // 120 is the pane's width already.
        assert_eq!(app.apply_term_resize(wid, 24, cols), cols != 120);
        draw_claude(&app, session, BYPASS);
        frame(&mut app, wid);
        let text = rule(&app, wid);
        assert_eq!(
            text.contains('\u{2713}'),
            text.contains("\u{2713} approved"),
            "{cols}: no lone glyph: {text:?}"
        );
        if text.contains("approved") {
            assert!(text.contains(WHOLE_FACTS), "{cols}: {text:?}");
            drawn.push(cols);
        }
        if painted(&app, wid, Chip::Autopilot) {
            assert_eq!(
                hit(&app, wid, Chip::Autopilot).1,
                at_left,
                "{cols}: the chips never move"
            );
        }
    }
    let least = *drawn.last().expect("CONTROL: drawn in a wide pane");
    assert_eq!(
        drawn,
        (least..=120).rev().collect::<Vec<_>>(),
        "one run, down to its width"
    );
    assert!(app.apply_term_resize(wid, 24, least - 1));
    draw_claude(&app, session, BYPASS);
    frame(&mut app, wid);
    let gone = rule(&app, wid);
    assert!(
        !gone.contains('\u{2713}') && gone.contains(WHOLE_FACTS),
        "{gone:?}"
    );
}

/// The Together run's Claude Code working directory (2026-10-16): outside
/// HOME, so the footer carries it whole, and a 120-column pane cuts it.
const SPLIT_PATH: &str = "/private/tmp/claude-502/-Users-person-aterm/\
    dd11ed0c-05cd-46e8-bf09-5b2786e48b6a/scratchpad/together/run-dir/proj-claude";

/// A SPLIT PANE'S CUT PATH LEAVES THE LABEL AND THE HOVERED TITLE ROOM
/// (§9.2.7; the Together run's D2, on glass): Claude Code's pane 120
/// columns wide in a vertical split, its working directory too long for the
/// rule, so the facts stand with the path cut to `…/proj-claude` and ninety
/// cells of `─` free. An act lights `✓ approved` there, and hovering the
/// Autopilot chip draws its title (v1's words: the whole, with the menu's
/// line, does not fit), each beside the facts exactly as the rest rule
/// draws them, the chips where they stood.
///
/// RED before the fix: the label and the title were paired with the WHOLE
/// facts only, so neither was drawn and a hover only underlined the chip.
/// NEGATIVE CONTROL: at 60 columns, where the cut facts leave no room for
/// it, the label is not drawn and leaves no lone glyph; the facts stand.
#[test]
fn a_split_panes_cut_path_leaves_the_label_and_the_hovered_title_room() {
    let t0 = Instant::now();
    let (mut app, wid, session, _reader) = app();
    let _hosted = host(&mut app);
    assert!(app.apply_term_resize(wid, 24, 120));
    publish_facts_at(&app, session, SPLIT_PATH);
    draw_claude(&app, session, BYPASS);
    frame(&mut app, wid);
    let facts = "\u{25C6} Opus 5.5 xhigh   \u{2302} \u{2026}/proj-claude   \u{2387} main \u{2500}";
    let rest = rule(&app, wid);
    assert!(
        rest.contains(CHIPS) && rest.contains(facts) && !rest.contains("together"),
        "CONTROL: at rest the path is cut: {rest:?}"
    );
    let at_left = hit(&app, wid, Chip::Autopilot).1;
    on_act_at(&mut app, session, Act::Approved, arrival(t0, 0));
    frame(&mut app, wid);
    let lit = rule(&app, wid);
    assert!(
        lit.contains(&format!("{CHIPS}  \u{2713} approved")),
        "the label beside the cut facts: {lit:?}"
    );
    assert!(lit.contains(facts), "no fact gave way: {lit:?}");
    assert_eq!(hit(&app, wid, Chip::Autopilot).1, at_left, "the chips stay");
    let end = app.harness_acts_deadline().expect("the label's end");
    app.expire_harness_acts_at(end);
    frame(&mut app, wid);
    assert_eq!(rule(&app, wid), rest, "the rest rule again");
    app.hover_claude_chip(wid, Some((session, Chip::Autopilot)));
    frame(&mut app, wid);
    let hovered = rule(&app, wid);
    assert!(
        hovered.contains(&format!(
            "{CHIPS}  autopilot: cruise \u{2014} keeps the work moving \u{b7} click: land "
        )),
        "the hovered title beside the cut facts: {hovered:?}"
    );
    assert!(hovered.contains(facts), "no fact gave way: {hovered:?}");
    app.hover_claude_chip(wid, None);
    // NEGATIVE CONTROL: where the cut facts leave it no room, no label.
    assert!(app.apply_term_resize(wid, 24, 60));
    draw_claude(&app, session, BYPASS);
    on_act_at(&mut app, session, Act::Declined, arrival(t0, 20_000));
    assert_eq!(
        app.chip_glass(session).lit,
        Some(Act::Declined),
        "CONTROL: the label is lit"
    );
    frame(&mut app, wid);
    let narrow = rule(&app, wid);
    assert!(
        !narrow.contains('\u{2717}') && narrow.contains(facts),
        "60 columns: no label, the facts stand: {narrow:?}"
    );
}

/// `[claude_footer] acts = false` AND SERIOUS MODE WITHHOLD THE LABEL
/// (§9.2.1) on the very next frame — the switch moves the footer's repaint
/// key, as `usage` does — and keep the trail: the hovered title still
/// lists the act. Back on, the label still lit shows again.
///
/// NEGATIVE CONTROL: each switch on, the label shows.
#[test]
fn acts_off_and_serious_mode_withhold_the_label_and_move_the_key() {
    let t0 = Instant::now();
    let switches: [Switch; 2] = [
        ("acts = false", |app, off| {
            app.config.claude_footer = Some(crate::app_config::ClaudeFooterConfig {
                acts: Some(!off),
                ..crate::app_config::ClaudeFooterConfig::default()
            });
        }),
        ("serious mode", |app, off| app.serious_mode = off),
    ];
    for (what, withhold) in switches {
        let (mut app, wid, session, _host) = ticker_app(120);
        on_act_at(&mut app, session, Act::Approved, arrival(t0, 0));
        frame(&mut app, wid);
        let lit_key = footer_fp(&app, wid);
        assert!(
            rule(&app, wid).contains("\u{2713} approved"),
            "CONTROL {what}"
        );
        withhold(&mut app, true);
        let off_key = footer_fp(&app, wid);
        assert_ne!(off_key, lit_key, "{what}: the key moves");
        frame(&mut app, wid);
        assert!(!rule(&app, wid).contains("approved"), "{what}: withheld");
        assert!(app.chip_glass(session).last.is_some(), "{what}: trail kept");
        withhold(&mut app, false);
        assert_eq!(footer_fp(&app, wid), lit_key, "{what}: back on");
        frame(&mut app, wid);
        assert!(
            rule(&app, wid).contains("\u{2713} approved"),
            "{what}: shown again"
        );
    }
}

/// A HOVERED AUTOPILOT CHIP LISTS THE TRAIL, BUILT ONCE PER READING
/// (§9.2.1): its whole title — which outranks the label — names the mode,
/// says what it does, then `last:` and the acts with their clock times,
/// then the click. The trail is formatted where the hover starts (one clock
/// read) and again where an act arrives under it (its generation moves the
/// key), never per frame; let go, nothing is read.
///
/// NEGATIVE CONTROL: before any act, the hovered title is v1's words and
/// the menu's line, no trail.
#[test]
fn a_hovered_autopilot_chip_lists_the_trail_built_once_per_reading() {
    let t0 = Instant::now();
    let (mut app, wid, session, _host) = ticker_app(200);
    app.hover_claude_chip(wid, Some((session, Chip::Autopilot)));
    frame(&mut app, wid);
    let v1 = rule(&app, wid);
    assert!(
        v1.contains(
            "autopilot: cruise \u{2014} keeps the work moving \u{b7} click: land \u{b7} \
             right-click: more"
        ) && !v1.contains("last:"),
        "CONTROL: {v1:?}"
    );
    app.hover_claude_chip(wid, None);
    frame(&mut app, wid);
    let (first, at) = arrival_now(t0, 0);
    on_act_at(&mut app, session, Act::Approved, first);
    reset_counts();
    app.hover_claude_chip(wid, Some((session, Chip::Autopilot)));
    for _ in 0..5 {
        frame(&mut app, wid);
    }
    // The act's clock time; a weekday before it only where the wall's date
    // turned between the act and the hover.
    let titled = rule(&app, wid);
    assert!(
        titled.contains(
            "autopilot: cruise \u{2014} keeps the work moving \u{b7} last: \u{2713} approved "
        ) && titled.contains(&format!("{at} \u{b7} click: land")),
        "the hover's title, not the label: {titled:?}"
    );
    assert!(!titled.contains("\u{2713} approved  "), "{titled:?}");
    assert_eq!(
        crate::work_counts::take_harness_clock_reads(),
        1,
        "the hover's start alone"
    );
    // An act under the hover: the key moves, the trail is built again.
    let key = footer_fp(&app, wid);
    let (second, _) = arrival_now(t0, 60_000);
    on_act_at(&mut app, session, Act::Continued, second);
    assert_ne!(
        footer_fp(&app, wid),
        key,
        "the trail moves a read title's key"
    );
    for _ in 0..5 {
        frame(&mut app, wid);
    }
    assert!(
        rule(&app, wid).contains("last: \u{21bb} kept going "),
        "{:?}",
        rule(&app, wid)
    );
    assert!(
        rule(&app, wid).contains(", \u{2713} approved "),
        "{:?}",
        rule(&app, wid)
    );
    assert_eq!(
        crate::work_counts::take_harness_clock_reads(),
        1,
        "rebuilt once"
    );
    // Let go: nothing read, the cache gone.
    app.hover_claude_chip(wid, None);
    for _ in 0..3 {
        frame(&mut app, wid);
    }
    assert_eq!(crate::work_counts::take_harness_clock_reads(), 0);
    assert!(app.windows[&wid].claude_lights.readings.is_empty());
}

/// A HOVERED CHIP NAMES ITS FACE CLOCK'S DAY WHERE IT IS NOT TODAY (§10.2.3,
/// item 3) on a real frame: a session with no trail whose task was done two
/// days back reads, hovered, `done <weekday> HH:MM` — the hover's start reads
/// today's day for the face clock alone, since the title names one.
///
/// NEGATIVE CONTROL: a face done now reads its bare clock, no weekday — the
/// day is named only where it differs.
#[test]
fn a_hovered_chip_names_its_face_clocks_day_where_it_is_not_today() {
    use crate::harness_glass::{Stamp, local_day_at};
    use aterm_agent::harness::acts::TaskFace;
    let wall_now = || {
        i64::try_from(
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .expect("after the epoch")
                .as_secs(),
        )
        .expect("a second")
    };
    let hovered_after = |done_unix: i64| {
        let (mut app, wid, session, host) = ticker_app(200);
        assert!(crate::harness_glass::on_task(
            &mut app,
            session,
            Some(TaskFace::Done),
            done_unix * 1000
        ));
        crate::claude_footer::post_changed(session);
        assert!(
            app.chip_glass(session).last.is_none(),
            "CONTROL: no trail, so only the face clock asks for the day"
        );
        app.hover_claude_chip(wid, Some((session, Chip::Autopilot)));
        frame(&mut app, wid);
        let text = rule(&app, wid);
        drop(host);
        text
    };
    let then = wall_now() - 2 * 86_400;
    let stamp = Stamp::at(then).expect("the zone reads here");
    let wkd = crate::claude_lights::Weekday::of_days(stamp.day).short();
    let old = hovered_after(then);
    assert!(
        old.contains(&format!("done {wkd} {}", stamp.clock.as_str())),
        "two days back, the weekday: {old:?}"
    );
    // NEGATIVE CONTROL: done now, the bare clock (where the wall's date did
    // not turn between the face and the hover).
    let now = wall_now();
    let stamp = Stamp::at(now).expect("the zone reads here");
    let fresh = hovered_after(now);
    if local_day_at(wall_now()) == stamp.day {
        let wkd = crate::claude_lights::Weekday::of_days(stamp.day).short();
        assert!(
            fresh.contains(&format!("done {}", stamp.clock.as_str()))
                && !fresh.contains(&format!("done {wkd} ")),
            "today, no weekday: {fresh:?}"
        );
    }
}

/// THE CHIPS ARE BUTTONS A SCREEN READER CAN PRESS (§9.2.1): the tree's
/// chips are the frame's — Autopilot and `⚙`, each over the cells the
/// painter recorded, named in words (`Autopilot: cruise.`, `Harness
/// settings`), the newest act Autopilot's description (none once it is a
/// day old, as the title leaves it out) — and a reader's click on Autopilot
/// steps its word, as a click on its cells does.
///
/// NEGATIVE CONTROL: a click on a chip the glass does not show (a pane too
/// narrow for it) does nothing.
#[cfg(a11y_tree)]
#[test]
fn the_chips_are_buttons_a_screen_reader_can_press() {
    use super::gesture_tests::autopilot;
    use crate::a11y_tree::HarnessChip;
    use crate::autopilot::AutopilotView;
    use aterm_agent::supervise::Autopilot;
    let t0 = Instant::now();
    let (mut app, wid, session, _host) = ticker_app(120);
    let (now, clock) = arrival_now(t0, 0);
    on_act_at(&mut app, session, Act::Approved, now);
    frame(&mut app, wid);
    let chips = app.claude_chips_a11y(wid);
    assert_eq!(chips.len(), 2, "{chips:?}");
    let strip = usize::from(app.chrome_rows(wid));
    let (row, col) = hit(&app, wid, Chip::Autopilot);
    assert_eq!(
        (chips[0].chip, chips[0].row, chips[0].col, chips[0].width),
        (HarnessChip::Autopilot, row - strip, col, 1)
    );
    assert_eq!(chips[0].label, "Autopilot: cruise.");
    assert_eq!(
        chips[0].description,
        Some(format!("Last: approved at {clock}."))
    );
    assert_eq!(
        (chips[1].chip, chips[1].label.as_str(), chips[1].col),
        (
            HarnessChip::Settings,
            "Harness settings",
            hit(&app, wid, Chip::Settings).1
        )
    );
    // A day and a minute old, the act is told no more.
    let (mut old, _) = arrival_now(t0, 20_000);
    old.unix -= 86_460;
    on_act_at(&mut app, session, Act::Continued, old);
    frame(&mut app, wid);
    assert_eq!(app.claude_chips_a11y(wid)[0].description, None, "a day old");
    // NEGATIVE CONTROL: a pane too narrow for any chip — a request for one
    // the glass does not show is a no-op.
    assert!(app.apply_term_resize(wid, 24, 8));
    draw_claude(&app, session, BYPASS);
    frame(&mut app, wid);
    assert!(
        app.claude_chips_a11y(wid).is_empty(),
        "no chip on the glass"
    );
    app.press_claude_chip_for_reader(wid, session, HarnessChip::Autopilot);
    assert_eq!(autopilot(&app, session), (None, AutopilotView::Cruise));
    assert!(app.apply_term_resize(wid, 24, 120));
    draw_claude(&app, session, BYPASS);
    frame(&mut app, wid);
    app.press_claude_chip_for_reader(wid, session, HarnessChip::Autopilot);
    assert_eq!(
        autopilot(&app, session),
        (Some(Autopilot::Land), AutopilotView::Land)
    );
}

#[cfg(a11y_tree)]
#[test]
fn a_reader_cannot_press_a_chip_under_painted_chrome() {
    use super::gesture_tests::{autopilot, feed};
    use crate::a11y_tree::HarnessChip;
    use crate::autopilot::AutopilotView;
    use crate::input::{PasteFraming, Source};
    use aterm_agent::supervise::Autopilot;

    let (mut app, wid, session, _host) = ticker_app(120);
    // The ordinary reader path really changes the session's word first.
    app.press_claude_chip_for_reader(wid, session, HarnessChip::Autopilot);
    assert_eq!(
        autopilot(&app, session),
        (Some(Autopilot::Land), AutopilotView::Land)
    );
    // A compact composer puts its footer under the portable paste banner.
    // Paint the actual band; never fabricate the chrome ownership register.
    assert!(app.apply_term_resize(wid, 6, 120));
    let rule = "\u{2500}".repeat(120);
    feed(
        &app,
        session,
        format!(
            "\x1b[2J\x1b[1;1H\u{25CF} done\x1b[3;1H{rule}\x1b[4;1H\u{276F} \
             \x1b[5;1H{rule}\x1b[6;1H{BYPASS}\x1b[4;3H"
        )
        .as_bytes(),
    );
    frame(&mut app, wid);
    let (row, col) = hit(&app, wid, Chip::Autopilot);
    assert!(!app.claude_chips_a11y(wid).is_empty());
    app.paste_banner = Some(crate::paste_banner::PendingPaste::new(
        wid,
        Some(session),
        "one\ntwo\nthree\nfour\nfive".to_owned(),
        Source::Controller,
        PasteFraming::Gesture { bracketed: true },
    ));
    app.splice_paste_banner(wid);
    let covered = &app.windows[&wid].claude_lights.hits;
    let term_row = covered
        .iter()
        .find(|h| h.session == session && h.chip == Some(Chip::Autopilot))
        .expect("the old painted hit remains")
        .term_row;
    assert!(app.chrome_owns_terminal_row(wid, term_row));
    assert!(app.claude_chips_a11y(wid).is_empty());
    let (cw, ch) = app.win_cell_size(wid);
    let (ox, oy) = app.frame_origin(wid);
    let x = ox as f64 + (app.win_pad(wid) + col * cw + cw / 2) as f64;
    let y = oy as f64 + (app.win_pad_top(wid) + app.win_head(wid) + row * ch + ch / 2) as f64;
    assert_eq!(app.claude_chips_at(wid, x, y), None);
    // A request saved from before the band must respect the same ownership.
    app.press_claude_chip_for_reader(wid, session, HarnessChip::Autopilot);
    assert_eq!(
        autopilot(&app, session),
        (Some(Autopilot::Land), AutopilotView::Land)
    );
}

#[cfg(a11y_tree)]
#[test]
fn a_reader_chip_click_commits_an_open_in_grid_rename() {
    use crate::a11y_tree::HarnessChip;
    use crate::app_rename::RenameSurface;
    use crate::app_search::SearchEdit;
    use crate::native_settings::SettingsRoute;
    use crate::session_timeline::MetaField;

    let (mut app, wid, session, _host) = ticker_app(120);
    app.tab_strip_rows = 1;
    frame(&mut app, wid);
    let tab = app.windows[&wid].tab_set.active_id().expect("Claude's tab");
    let pinned = |app: &App| {
        app.pool
            .get(session)
            .expect("session")
            .ctx
            .meta
            .lock()
            .unwrap_or_else(|p| p.into_inner())
            .presentation_value(MetaField::Title.wire_name())
    };
    let rename = |app: &mut App, name: &str| {
        assert!(app.begin_session_rename(wid, tab));
        assert_eq!(
            app.inline_rename_edit(wid).map(|edit| edit.surface),
            Some(RenameSurface::InGrid)
        );
        assert!(app.rename_field_edit(wid, SearchEdit::KillToStart));
        assert!(app.rename_field_edit(wid, SearchEdit::Insert(name.to_owned())));
    };
    // Merely switching to Settings intentionally leaves the edit uncommitted.
    rename(&mut app, "left open");
    assert!(app.open_harness_settings(wid, crate::harness_screen::HarnessFocus::Top));
    assert!(app.inline_rename_edit(wid).is_some());
    app.end_session_rename_editor(wid);
    assert_eq!(pinned(&app), None);
    app.switch_tab_in(wid, 0);
    frame(&mut app, wid);
    rename(&mut app, "reader gear");
    app.press_claude_chip_for_reader(wid, session, HarnessChip::Settings);
    let (at, _, view, _) = app
        .native_settings_route_view_target(SettingsRoute::Harness)
        .expect("the reader opened Settings");
    assert_eq!(at, wid);
    assert_eq!(
        app.windows[&wid].tab_set.active().map(|tab| tab.focus),
        Some(view),
        "the reader actually focused Settings"
    );
    assert!(app.inline_rename_edit(wid).is_none());
    assert_eq!(pinned(&app).as_deref(), Some("reader gear"));
}

/// A HOVERED TITLE WITH A TRAIL GIVES ITS PARTS UP IN ORDER (§9.2.1; review
/// of fix/autopilot-menu-wiring, finding 2): walked from 240 columns down to
/// 60 with one act taken, the hovered Autopilot title beside the whole facts
/// is the WHOLE (the trail and the menu's line), then the whole WITHOUT ITS
/// TRAIL (the menu still offered), then V1'S WORDS (the menu's line given up
/// too), then the SHORT form, then none — in that order as the pane
/// narrows, never back; the facts stay whole beside each. CONTROL: each of
/// the five forms is met.
///
/// RED before the fix: a pane with no room for the trail beside the facts
/// showed `autopilot: cruise` alone, and the menu's line went with the
/// trail.
#[test]
fn a_hovered_title_gives_up_its_trail_then_its_menu_line_then_its_words() {
    let t0 = Instant::now();
    let (mut app, wid, session, _host) = ticker_app(241);
    let (act, at) = arrival_now(t0, 0);
    on_act_at(&mut app, session, Act::Approved, act);
    let v1 = "autopilot: cruise \u{2014} keeps the work moving \u{b7} click: land";
    let menu = format!("{v1} \u{b7} right-click: more");
    let tail = format!("{at} \u{b7} click: land \u{b7} right-click: more");
    let mut seen: Vec<(u16, u8)> = Vec::new();
    for width in (60..=240u16).rev() {
        assert!(app.apply_term_resize(wid, 24, width));
        draw_claude(&app, session, BYPASS);
        app.hover_claude_chip(wid, None);
        frame(&mut app, wid);
        app.hover_claude_chip(wid, Some((session, Chip::Autopilot)));
        frame(&mut app, wid);
        let rule = rule(&app, wid);
        let form: u8 = if rule.contains(
            "autopilot: cruise \u{2014} keeps the work moving \u{b7} last: \u{2713} approved ",
        ) && rule.contains(&tail)
        {
            4
        } else if rule.contains(&menu) {
            assert!(!rule.contains("last:"), "{width}: {rule:?}");
            3
        } else if rule.contains(v1) {
            assert!(!rule.contains("right-click"), "{width}: {rule:?}");
            2
        } else if rule.contains(&format!("{CHIPS}  autopilot: cruise ")) {
            assert!(
                !rule.contains("click") && !rule.contains("\u{2014}"),
                "{width}: the mode phrase alone: {rule:?}"
            );
            1
        } else {
            assert!(
                !rule.contains("autopilot"),
                "{width}: no cut title: {rule:?}"
            );
            0
        };
        if form > 0 {
            assert!(
                rule.contains(WHOLE_FACTS),
                "{width}: the facts whole: {rule:?}"
            );
        }
        if let Some(&(wider, last)) = seen.last() {
            assert!(
                form <= last,
                "{width} after {wider}: a form came back: {rule:?}"
            );
        }
        seen.push((width, form));
    }
    let met = |form: u8| seen.iter().any(|(_, f)| *f == form);
    assert!((0..=4).all(met), "CONTROL: every form is met: {seen:?}");
    let from = |form: u8| {
        seen.iter()
            .filter(|(_, f)| *f == form)
            .map(|(w, _)| *w)
            .min()
    };
    eprintln!(
        "with one act: whole from {:?}, without the trail from {:?}, v1's words from {:?}, \
         short from {:?}",
        from(4),
        from(3),
        from(2),
        from(1)
    );
}
