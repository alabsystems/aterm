// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Act ticker tests on a real frame.

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

type Switch = (&'static str, fn(&mut App, bool));

const CHIPS: &str = "\u{221e}";

fn ticker_app(cols: u16) -> (App, WindowId, u64, (Hosted, UnixStream)) {
    let (mut app, wid, session, reader) = app();
    let hosted = host(&mut app);
    assert!(app.apply_term_resize(wid, 24, cols));
    publish_facts(&app, session);
    draw_claude(&app, session, BYPASS);
    frame(&mut app, wid);
    (app, wid, session, (hosted, reader))
}

fn rule(app: &App, wid: WindowId) -> String {
    row_text(app, wid, footer_row(app, wid))
}

fn arrival(t0: Instant, ms: u64) -> Arrival {
    Arrival {
        now: t0 + Duration::from_millis(ms),
        unix: 1_792_159_500,
        offset_s: Some(0),
    }
}

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

fn reset_counts() {
    let _ = (
        crate::work_counts::take_footer_rules_composed(),
        crate::work_counts::take_harness_clock_reads(),
    );
}

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
    let check = cell_at(&app, wid, (row, col + 3));
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

#[test]
fn the_acts_label_is_drawn_beside_the_whole_facts_or_not_at_all() {
    let t0 = Instant::now();
    let (mut app, wid, session, _host) = ticker_app(120);
    on_act_at(&mut app, session, Act::Approved, arrival(t0, 0));
    let at_left = hit(&app, wid, Chip::Autopilot).1;
    let mut drawn = Vec::new();
    for cols in (40..=120).rev() {
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

const SPLIT_PATH: &str = "/private/tmp/claude-502/-Users-person-aterm/\
    dd11ed0c-05cd-46e8-bf09-5b2786e48b6a/scratchpad/together/run-dir/proj-claude";

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
    let (whole, _) = super::autopilot_title(
        crate::autopilot::AutopilotView::Cruise,
        &app.chip_glass(session),
        "",
        None,
        super::TitleWords::default(),
    );
    assert!(
        super::lean_titles(&whole)
            .iter()
            .filter(|(head, _)| !head.is_empty())
            .any(|(head, tail)| hovered.contains(&format!("{CHIPS}  {head}{tail} "))),
        "a leaner form of the hovered title beside the cut facts: {hovered:?}"
    );
    assert!(hovered.contains(facts), "no fact gave way: {hovered:?}");
    app.hover_claude_chip(wid, None);
    assert!(app.apply_term_resize(wid, 24, 58));
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
        "58 columns: no label, the facts stand: {narrow:?}"
    );
}

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

#[test]
fn a_hovered_autopilot_chip_lists_the_trail_built_once_per_reading() {
    let t0 = Instant::now();
    let (mut app, wid, session, _host) = ticker_app(200);
    app.hover_claude_chip(wid, Some((session, Chip::Autopilot)));
    frame(&mut app, wid);
    let v1 = rule(&app, wid);
    assert!(
        v1.contains(&format!(
            "{} \u{b7} right-click: more",
            crate::autopilot::AutopilotView::Cruise.title()
        )) && !v1.contains("last:"),
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
    let titled = rule(&app, wid);
    assert!(
        titled.contains(&format!(
            "autopilot: cruise \u{2014} {} \u{b7} last: \u{2713} approved ",
            crate::autopilot::mode_words(aterm_agent::supervise::Autopilot::Cruise).chip
        )) && titled.contains(&format!("{at} \u{b7} click: land")),
        "the hover's title, not the label: {titled:?}"
    );
    assert!(!titled.contains("\u{2713} approved  "), "{titled:?}");
    assert_eq!(
        crate::work_counts::take_harness_clock_reads(),
        1,
        "the hover's start alone"
    );
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
    app.hover_claude_chip(wid, None);
    for _ in 0..3 {
        frame(&mut app, wid);
    }
    assert_eq!(crate::work_counts::take_harness_clock_reads(), 0);
    assert!(app.windows[&wid].claude_lights.readings.is_empty());
}

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
        assert!(app.set_session_autopilot(
            session,
            Some(aterm_agent::supervise::Autopilot::Land),
            crate::autopilot::Surface::Chip
        ));
        assert!(crate::harness_glass::on_task(
            &mut app,
            session,
            Some(TaskFace::Landed),
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
        old.contains(&format!("landed {wkd} {}", stamp.clock.as_str())),
        "two days back, the weekday: {old:?}"
    );
    let now = wall_now();
    let stamp = Stamp::at(now).expect("the zone reads here");
    let fresh = hovered_after(now);
    if local_day_at(wall_now()) == stamp.day {
        let wkd = crate::claude_lights::Weekday::of_days(stamp.day).short();
        assert!(
            fresh.contains(&format!("landed {}", stamp.clock.as_str()))
                && !fresh.contains(&format!("landed {wkd} ")),
            "today, no weekday: {fresh:?}"
        );
    }
}

#[test]
fn a_pause_told_under_a_hover_is_in_the_title_on_the_next_frame() {
    use crate::harness_glass::Stamp;
    use crate::work_counts::take_harness_clock_reads;
    let asked = || crate::claude_lights::TITLE_READERS_ASKED.with(|a| a.replace(0));
    let wall_now = || {
        i64::try_from(
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .expect("after the epoch")
                .as_secs(),
        )
        .expect("a second")
    };
    let (mut app, wid, session, _host) = ticker_app(200);
    let sid = app
        .pool
        .get(session)
        .expect("the session")
        .ctx
        .self_id
        .as_str()
        .to_string();
    let glass = app.chip_glass(session);
    assert!(
        glass.last.is_none() && glass.task_at.is_none(),
        "CONTROL: no trail and no face clock, so only the pause asks for the day"
    );
    let at_unix = wall_now() + 2 * 86_400;
    let plan = crate::PausePlan { at_unix, short: 3 };
    let until = Stamp::at(at_unix).expect("the zone reads here");
    let wkd = crate::claude_lights::Weekday::of_days(until.day).short();
    let words = format!(
        "pausing until {wkd} {} after 3 short turns",
        until.clock.as_str()
    );

    asked();
    app.apply_harness_fact(&sid, crate::HarnessFact::Pause(Some(plan)));
    assert_eq!(asked(), 0, "no reader, no frame asked");
    app.apply_harness_fact(&sid, crate::HarnessFact::Pause(None));
    assert_eq!(asked(), 0);

    app.hover_claude_chip(wid, Some((session, Chip::Autopilot)));
    frame(&mut app, wid);
    assert!(
        !rule(&app, wid).contains("pausing until"),
        "{:?}",
        rule(&app, wid)
    );
    take_harness_clock_reads();
    app.apply_harness_fact(&sid, crate::HarnessFact::Pause(Some(plan)));
    assert_eq!(asked(), 1, "the reading window is asked for a frame");
    frame(&mut app, wid);
    let titled = rule(&app, wid);
    assert!(titled.contains(&words), "want {words:?} in {titled:?}");
    assert_eq!(
        take_harness_clock_reads(),
        1,
        "built again once, one wall read"
    );
    app.apply_harness_fact(&sid, crate::HarnessFact::Pause(None));
    assert_eq!(asked(), 1);
    frame(&mut app, wid);
    assert!(
        !rule(&app, wid).contains("pausing until"),
        "{:?}",
        rule(&app, wid)
    );
}

/// THE CHIPS ARE BUTTONS A SCREEN READER CAN PRESS (§9.2.1): the tree's
/// chips are the frame's — Autopilot (then the facts), over the cells the
/// painter recorded, named in words (`Autopilot: cruise.`), the newest act
/// its description (none once it is a day old, as the title leaves it out)
/// — and a reader's click on Autopilot steps its word, as a click on its
/// cells does.
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
    assert!(
        chips.len() > 1
            && chips[1..].iter().all(|c| c.chip.fact().is_some()
                || matches!(
                    c.chip,
                    HarnessChip::Switch | HarnessChip::SwitchOn | HarnessChip::Title
                )),
        "{chips:?}"
    );
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
    let (mut old, _) = arrival_now(t0, 20_000);
    old.unix -= 86_460;
    on_act_at(&mut app, session, Act::Continued, old);
    frame(&mut app, wid);
    assert_eq!(app.claude_chips_a11y(wid)[0].description, None, "a day old");
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
    app.press_claude_chip_for_reader(wid, session, HarnessChip::Autopilot);
    assert_eq!(
        autopilot(&app, session),
        (Some(Autopilot::Land), AutopilotView::Land)
    );
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
    app.press_claude_chip_for_reader(wid, session, HarnessChip::Autopilot);
    assert_eq!(
        autopilot(&app, session),
        (Some(Autopilot::Land), AutopilotView::Land)
    );
}

/// EVERYTHING ATERM DRAWS IN THE RULES IS IN THE TREE (review G1): the
/// grid's text there is Claude's own `─`, so the permission-mode light, the
/// top rule's harness switch and the title slot are nodes of their own.
#[cfg(a11y_tree)]
#[test]
fn the_mode_light_the_switch_and_the_title_slot_are_in_the_tree() {
    use super::gesture_tests::{MODE, PGID, PLAN, autopilot, pty, px_of};
    use crate::a11y_tree::{GridChip, HarnessChip};
    use crate::autopilot::AutopilotView;
    use aterm_agent::harness::lights::{Mode, SHIFT_TAB};
    use aterm_agent::supervise::Autopilot;
    let of = |chips: &[GridChip], chip| chips.iter().find(|c| c.chip == chip).cloned();
    let (mut app, wid, session, (_hosted, mut reader)) = ticker_app(120);
    {
        let ctx = app.pool.get(session).expect("session").ctx.clone();
        {
            let mut timeline = ctx.timeline.lock().unwrap_or_else(|p| p.into_inner());
            timeline.note_foreground_group(PGID);
            timeline.set_program(PGID, Some("claude".into()));
        }
        crate::session_timeline::claim_supervisor(
            &ctx,
            &crate::harness_host::holder(),
            None,
            None,
            crate::metrics::now_us(),
        )
        .expect("the claim");
    }
    draw_claude(&app, session, BYPASS);
    frame(&mut app, wid);
    let strip = usize::from(app.chrome_rows(wid));
    assert_eq!(of(&app.claude_chips_a11y(wid), HarnessChip::Mode), None);
    app.press_claude_chip_for_reader(wid, session, HarnessChip::Mode);
    assert!(pty(&mut reader, 0).is_empty(), "no light, nothing typed");
    draw_claude(&app, session, PLAN);
    frame(&mut app, wid);
    let chips = app.claude_chips_a11y(wid);
    let light = of(&chips, HarnessChip::Mode).unwrap_or_else(|| panic!("{chips:?}"));
    let (row, col) = hit(&app, wid, MODE);
    assert_eq!((light.row, light.col), (row - strip, col));
    assert_eq!(light.label, "Permission mode: plan.");
    let relabelled = app.windows[&wid].claude_lights.sessions[&session].relabelled;
    assert_eq!(
        light.description,
        Some(format!(
            "Click for {} or auto.",
            Mode::Bypass.word_on_glass(relabelled)
        ))
    );
    let switch = of(&chips, HarnessChip::Switch).unwrap_or_else(|| panic!("{chips:?}"));
    assert_eq!(switch.label, "alab harness on: Claude Code");
    assert_eq!(switch.toggled, Some(aterm_a11y::model::Toggled::True));
    assert!(switch.description.is_some(), "{switch:?}");
    let drawn = app.windows[&wid]
        .claude_lights
        .border_toggles
        .iter()
        .find(|b| b.session == session)
        .cloned()
        .expect("the switch is drawn");
    assert_eq!(
        (switch.row, switch.col, switch.width),
        (
            drawn.frame_row - strip,
            drawn.switch.start,
            drawn.switch.len()
        )
    );
    assert!(
        row_text(&app, wid, drawn.frame_row)[..].contains("alab harness \u{25cf} on"),
        "the glass says what the node says"
    );
    assert_eq!(of(&chips, HarnessChip::Title), None, "{chips:?}");
    let (x, y) = px_of(&app, wid, (row, col));
    app.on_cursor_moved(wid, x, y);
    frame(&mut app, wid);
    let title = of(&app.claude_chips_a11y(wid), HarnessChip::Title).expect("the title slot");
    assert_eq!(title.row, row - strip);
    assert!(
        title.label.starts_with("Permission mode: "),
        "{:?}",
        title.label
    );
    assert!(
        rule(&app, wid).contains(&title.label),
        "the slot's words as drawn: {:?} in {:?}",
        title.label,
        rule(&app, wid)
    );
    app.press_claude_chip_for_reader(wid, session, HarnessChip::Mode);
    assert_eq!(pty(&mut reader, SHIFT_TAB.len()), SHIFT_TAB);
    app.press_claude_chip_for_reader(wid, session, HarnessChip::Switch);
    assert_eq!(
        autopilot(&app, session),
        (Some(Autopilot::Off), AutopilotView::Off)
    );
    frame(&mut app, wid);
    let chips = app.claude_chips_a11y(wid);
    let switch = of(&chips, HarnessChip::Switch).expect("the off switch");
    assert_eq!(switch.label, "alab harness off: Claude Code");
    assert_eq!(switch.toggled, Some(aterm_a11y::model::Toggled::False));
    let on = of(&chips, HarnessChip::SwitchOn).expect("the on button");
    assert_eq!(on.label, "on: turn the Claude Code harness on");
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
    rename(&mut app, "left open");
    assert!(app.open_harness_settings(wid, crate::harness_screen::HarnessFocus::Top));
    let (_, _, settings, _) = app
        .native_settings_route_view_target(SettingsRoute::Harness)
        .expect("the control's Settings tab");
    assert!(app.inline_rename_edit(wid).is_some());
    app.end_session_rename_editor(wid);
    assert_eq!(pinned(&app), None);
    app.switch_tab_in(wid, 0);
    frame(&mut app, wid);
    app.harness_policy = crate::harness_policy_cell::HarnessPolicyCell::new(
        crate::app_config::HarnessPolicy::read("[harness]\nenabled = false\n"),
    );
    frame(&mut app, wid);
    rename(&mut app, "reader off chip");
    app.press_claude_chip_for_reader(wid, session, HarnessChip::Autopilot);
    assert_ne!(
        app.windows[&wid].tab_set.active().map(|tab| tab.focus),
        Some(settings),
        "the reader's click raised no Settings"
    );
    assert_eq!(app.focused_session_id(wid), Some(session));
    assert!(app.inline_rename_edit(wid).is_none());
    assert_eq!(pinned(&app).as_deref(), Some("reader off chip"));
}

#[test]
fn a_hovered_title_gives_up_its_trail_then_its_menu_line_then_its_words() {
    let t0 = Instant::now();
    let (mut app, wid, session, _host) = ticker_app(241);
    let (act, at) = arrival_now(t0, 0);
    on_act_at(&mut app, session, Act::Approved, act);
    let v1 = &crate::autopilot::AutopilotView::Cruise.title();
    let menu = format!("{v1} \u{b7} right-click: more");
    let cruise = crate::autopilot::mode_words(aterm_agent::supervise::Autopilot::Cruise).chip;
    let click = "autopilot: cruise \u{b7} click: land";
    let clicks = format!("{click} \u{b7} right-click: more");
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
        let form: u8 = if rule.contains(&format!(
            "autopilot: cruise \u{2014} {cruise} \u{b7} last: \u{2713} approved "
        )) && rule.contains(&tail)
        {
            6
        } else if rule.contains(&menu) {
            assert!(!rule.contains("last:"), "{width}: {rule:?}");
            5
        } else if rule.contains(v1) {
            assert!(!rule.contains("right-click"), "{width}: {rule:?}");
            4
        } else if rule.contains(&clicks) {
            assert!(
                !rule.contains("last:") && !rule.contains("\u{2014}"),
                "{width}: {rule:?}"
            );
            3
        } else if rule.contains(click) {
            assert!(
                !rule.contains("right-click") && !rule.contains("\u{2014}"),
                "{width}: {rule:?}"
            );
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
    assert!((0..=6).all(met), "CONTROL: every form is met: {seen:?}");
    let from = |form: u8| {
        seen.iter()
            .filter(|(_, f)| *f == form)
            .map(|(w, _)| *w)
            .min()
    };
    eprintln!(
        "with one act: whole from {:?}, without the trail from {:?}, v1's words from {:?}, \
         both clicks from {:?}, the click from {:?}, short from {:?}",
        from(6),
        from(5),
        from(4),
        from(3),
        from(2),
        from(1)
    );
}
