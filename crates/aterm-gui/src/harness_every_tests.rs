// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! SESSIONS' `Every session` row on the App (design §11.5.5): the press's
//! rule over live sessions, the one instant a timer arms, the default handed
//! back as a row's press hands it back, and the page published once.

use super::*;
use crate::autopilot_timers::{TimerChoice, set_fake_clocks};

/// A test App with five live agent sessions under a default of Cruise (the
/// design's case, §11.5.5): two following the default, one pinned Off, one
/// under Land by its own word, and one under its agent's own switch off (an
/// agy session, `[harness.agy] enabled = false`, the window's harness on).
/// `[following, following, pinned off, land, switched off]`.
fn five_sessions(policy: &str) -> (App, crate::harness_host::HostHandle, [u64; 5]) {
    let mut app = App::headless_for_test();
    let wid = WindowId(0);
    let mut ids = vec![0];
    for _ in 0..4 {
        let s = app.next_session_id;
        app.push_stub_tab(wid, crate::stub_session(s));
        ids.push(s);
    }
    let ids: [u64; 5] = ids.try_into().expect("five");
    for &s in &ids[..4] {
        run_claude(&app, s);
    }
    {
        let ctx = app.pool.get(ids[4]).unwrap().ctx.clone();
        let mut tl = ctx.timeline.lock().unwrap();
        tl.note_foreground_group(7);
        tl.set_program(7, Some("agy".into()));
    }
    let host = idle_host();
    app.harness = Some(host.clone());
    app.harness_policy =
        crate::harness_policy_cell::HarnessPolicyCell::new(crate::app_config::HarnessPolicy::read(
            &format!("[harness.agy]\nenabled = false\n{policy}"),
        ));
    assert!(app.set_session_autopilot(
        ids[2],
        Some(Autopilot::Off),
        crate::autopilot::Surface::Settings
    ));
    assert!(app.set_session_autopilot(
        ids[3],
        Some(Autopilot::Land),
        crate::autopilot::Surface::Settings
    ));
    assert_eq!(
        app.session_harness_posture(ids[4]),
        HarnessPosture::AgentOff(aterm_phase::Program::Agy),
        "the fifth is under its agent's own switch off"
    );
    assert_eq!(
        app.harness_state().sessions.len(),
        5,
        "SESSIONS lists all five"
    );
    (app, host, ids)
}

/// Each session's own word and effective view.
fn words(app: &App, ids: &[u64]) -> Vec<(Option<Autopilot>, AutopilotView)> {
    ids.iter()
        .map(|&s| {
            let (word, view, _) = app.session_autopilot(s);
            (word, view)
        })
        .collect()
}

/// How many keyed Autopilot records session `s` has in the message log.
fn records(app: &App, s: u64) -> usize {
    let key = format!(
        "harness.autopilot.{}",
        app.pool.get(s).unwrap().ctx.self_id.as_str()
    );
    app.messages
        .log()
        .records()
        .filter(|r| r.key.as_deref() == Some(key.as_str()))
        .count()
}

/// Each session's stored `land_at`.
fn timers(app: &App, ids: &[u64]) -> Vec<Option<i64>> {
    ids.iter()
        .map(|&s| app.pool.get(s).unwrap().ctx.meta.lock().unwrap().land_at)
        .collect()
}

/// A PRESS FOR EVERY SESSION NEVER LIFTS AN OFF ONE (design §11.5.5): of
/// five sessions under a default of Cruise, Land every session lands the two
/// that cruise; the one pinned Off, the one already landing and the one under
/// its agent's own switch off are untouched, and the keyed records go only to
/// the two moved. Cruise every session then lifts the Lands (the bulk's and
/// the one set by hand) and leaves both Offs off; a timer and Follow the
/// default leave them too. NEGATIVE CONTROL (run by hand, recorded in the
/// landing commit): a bulk Cruise whose rule also moves a session under Off
/// fails here.
#[test]
fn a_press_for_every_session_never_lifts_an_off_one() {
    use crate::harness_screen::EveryPress;
    let (mut app, host, ids) = five_sessions("");
    let [a, b, off, land, agy] = ids;
    let before: Vec<usize> = ids.iter().map(|&s| records(&app, s)).collect();
    assert_eq!(
        app.press_autopilot_every(EveryPress::Mode(Autopilot::Land)),
        2,
        "the two that cruise"
    );
    assert_eq!(
        words(&app, &ids),
        [
            (Some(Autopilot::Land), AutopilotView::Land),
            (Some(Autopilot::Land), AutopilotView::Land),
            (Some(Autopilot::Off), AutopilotView::Off),
            (Some(Autopilot::Land), AutopilotView::Land),
            (None, AutopilotView::AgentOff(aterm_phase::Program::Agy)),
        ]
    );
    let after: Vec<usize> = ids.iter().map(|&s| records(&app, s)).collect();
    assert_eq!(
        after
            .iter()
            .zip(&before)
            .map(|(after, before)| after - before)
            .collect::<Vec<_>>(),
        [1, 1, 0, 0, 0],
        "a record for each session moved, and none for the rest"
    );
    assert_eq!(
        app.press_autopilot_every(EveryPress::Mode(Autopilot::Cruise)),
        3,
        "the two landed and the one set by hand"
    );
    for (s, (_, view)) in [a, b, land].into_iter().zip(words(&app, &[a, b, land])) {
        assert_eq!(view, AutopilotView::Cruise, "session {s} cruises again");
    }
    for press in [
        EveryPress::Timer(TimerChoice::In(60)),
        EveryPress::Follow,
        EveryPress::Mode(Autopilot::Cruise),
    ] {
        app.press_autopilot_every(press);
        assert_eq!(
            words(&app, &[off, agy]),
            [
                (Some(Autopilot::Off), AutopilotView::Off),
                (None, AutopilotView::AgentOff(aterm_phase::Program::Agy)),
            ],
            "{press:?}: both Offs stay off"
        );
    }
    assert_eq!(timers(&app, &[off, agy]), [None, None]);
    host.shutdown_and_join();
}

/// THE ROW UNDOES ITS OWN PRESSES (design §11.5.1): a bulk Land then a bulk
/// Cruise gives back each session that was cruising; a bulk timer then a
/// bulk Cruise leaves no timer stored and none indexed. NEGATIVE CONTROL
/// (run by hand): a Cruise that moves only sessions under Land leaves the
/// timers armed, and fails here.
#[test]
fn a_bulk_cruise_reverses_a_bulk_land_and_a_bulk_timer() {
    use crate::harness_screen::EveryPress;
    let (mut app, host, ids) = five_sessions("");
    let [a, b, ..] = ids;
    let cruising = words(&app, &[a, b]);
    assert_eq!(
        cruising,
        [(None, AutopilotView::Cruise), (None, AutopilotView::Cruise)]
    );
    app.press_autopilot_every(EveryPress::Mode(Autopilot::Land));
    app.press_autopilot_every(EveryPress::Mode(Autopilot::Cruise));
    assert_eq!(words(&app, &[a, b]), cruising, "as they were");

    assert_eq!(
        app.press_autopilot_every(EveryPress::Timer(TimerChoice::In(120))),
        3,
        "every session under Cruise, the hand's Land lifted above"
    );
    assert_eq!(app.autopilot_timers.ids().len(), 3);
    assert_eq!(
        app.press_autopilot_every(EveryPress::Mode(Autopilot::Cruise)),
        3,
        "every timed Cruise"
    );
    assert_eq!(timers(&app, &ids), [None; 5], "no timer stored");
    assert!(app.autopilot_timers.is_empty(), "and none indexed");
    for (_, view) in words(&app, &[a, b]) {
        assert_eq!(view, AutopilotView::Cruise);
    }
    host.shutdown_and_join();
}

/// A TIMER FOR EVERY SESSION ARMS ONE INSTANT (design §11.5.3, item 4): the
/// press resolves `2 h` ONCE — one wall read, counted where it is made
/// (`resolve_timer_choice`) — however many sessions it arms, so every
/// session under Cruise stores the same `land_at`, the index holds one
/// instant, and the loop folds one `autopilot_timer` deadline for them all;
/// the Off, the Land and the switched-off sessions are not armed. The
/// rows' own Timer, pressed on each in turn, reads the wall once a session,
/// which the count tells apart. NEGATIVE CONTROL (run by hand, recorded in
/// the landing commit): the press resolving the instant per write
/// (`resolve_timer_choice` inside the per-session loop) reads it three
/// times, and fails here.
#[test]
fn a_timer_for_every_session_arms_one_instant() {
    use crate::harness_screen::EveryPress;
    let (mut app, host, ids) = five_sessions("");
    let [a, b, off, land, agy] = ids;
    // A third cruising session: the hand's Land lifted by a row's press.
    assert!(app.press_page_mode(land, Autopilot::Cruise));
    let now = std::time::Instant::now();
    let wall = 1_792_177_230_000_i64;
    set_fake_clocks(Some((now, wall)));
    crate::work_counts::take_timer_resolves();
    assert_eq!(
        app.press_autopilot_every(EveryPress::Timer(TimerChoice::In(120))),
        3
    );
    assert_eq!(
        crate::work_counts::take_timer_resolves(),
        1,
        "one wall read for three sessions"
    );
    let stored = timers(&app, &ids);
    assert_eq!(stored[2], None, "the Off one");
    assert_eq!(stored[4], None, "the switched-off one");
    let instants: std::collections::BTreeSet<i64> = stored.iter().flatten().copied().collect();
    assert_eq!(
        instants.into_iter().collect::<Vec<_>>(),
        [wall + 2 * 3_600_000],
        "one instant for every Cruise session"
    );
    assert_eq!(app.autopilot_timers.ids(), [a, b, land]);
    let dues: std::collections::BTreeSet<std::time::Instant> = [a, b, land]
        .iter()
        .map(|&s| app.autopilot_timers.entry(s).expect("armed").due)
        .collect();
    assert_eq!(dues.len(), 1, "one deadline folds them all");
    assert_eq!(app.autopilot_timer_deadline(), dues.first().copied());
    let _ = (off, agy);

    // The rows' own Timer, one session at a time: a wall read each, so a
    // clock that crosses a minute between two of them arms two instants.
    assert_eq!(
        app.press_autopilot_every(EveryPress::Mode(Autopilot::Cruise)),
        3
    );
    for (i, &s) in [a, b, land].iter().enumerate() {
        let moved = if i == 0 { 0 } else { 61_000 };
        set_fake_clocks(Some((now, wall + moved)));
        assert!(app.choose_autopilot_timer(
            s,
            TimerChoice::In(120),
            crate::autopilot::Surface::Settings
        ));
    }
    assert_eq!(
        crate::work_counts::take_timer_resolves(),
        3,
        "a row's Timer reads the wall per session"
    );
    let instants: std::collections::BTreeSet<i64> =
        timers(&app, &ids).into_iter().flatten().collect();
    assert_eq!(instants.len(), 2, "and arms two instants");
    set_fake_clocks(None);
    host.shutdown_and_join();
}

/// A TIMER THAT NAMES NO INSTANT WRITES NOTHING (design §11.5.3, item 4):
/// `at` pressed where `[harness] land_at` names no time (the line removed
/// after the page was drawn, a zone that cannot be read) moves no session,
/// and every timer stored stays as it was. NEGATIVE CONTROL (run by hand,
/// recorded in the landing commit): the press without its no-instant guard
/// writes a plain Cruise over each timed one, cancelling both timers, and
/// fails here.
#[test]
fn a_timer_at_no_instant_moves_nothing() {
    use crate::harness_screen::EveryPress;
    let (mut app, host, ids) = five_sessions("");
    let [a, b, ..] = ids;
    assert_eq!(
        app.press_autopilot_every(EveryPress::Timer(TimerChoice::In(120))),
        2
    );
    let armed = timers(&app, &ids);
    assert!(armed[0].is_some() && armed[1].is_some(), "{armed:?}");
    let before = words(&app, &ids);
    assert_eq!(
        app.press_autopilot_every(EveryPress::Timer(TimerChoice::At)),
        0,
        "no land_at: no instant, no write"
    );
    assert_eq!(timers(&app, &ids), armed, "every timer as it was");
    assert_eq!(words(&app, &ids), before);
    assert_eq!(app.autopilot_timers.ids(), [a, b]);
    host.shutdown_and_join();
}

/// LAND AND FOLLOW SAY THE TIMERS THEY CANCEL (design §11.5.1): with two
/// sessions timed, Land every session's words count the two timers it
/// cancels (`Land 2 sessions and cancel 2 timers`, spoken `…, cancelling 2
/// timers`), and Follow the default's too; after the Land, a Cruise gives
/// Cruise back but not the timers. NEGATIVE CONTROL (run by hand, recorded
/// in the landing commit): words that do not count the timers
/// (`EveryRow::cancels` answering 0) fail here.
#[test]
fn land_and_follow_say_the_timers_they_cancel() {
    use crate::harness_screen::EveryPress;
    let (mut app, host, ids) = five_sessions("");
    let [a, b, ..] = ids;
    app.press_autopilot_every(EveryPress::Timer(TimerChoice::In(120)));
    let every = app.harness_state().every_row().expect("five sessions");
    let land = EveryPress::Mode(Autopilot::Land);
    assert_eq!(
        every.description(land),
        "Land 2 sessions and cancel 2 timers; the 2 off stay off"
    );
    assert_eq!(every.spoken_tail(land), ", cancelling 2 timers");
    assert_eq!(
        every.description(EveryPress::Follow),
        "3 sessions follow [harness] autopilot again, 2 timers cancelled; the 1 turned off stays \
         off"
    );
    assert_eq!(
        every.spoken_tail(EveryPress::Follow),
        ", cancelling 2 timers"
    );
    assert_eq!(app.press_autopilot_every(land), 2);
    assert_eq!(timers(&app, &[a, b]), [None, None], "the timers cancelled");
    assert_eq!(
        app.press_autopilot_every(EveryPress::Mode(Autopilot::Cruise)),
        3,
        "the two landed and the one set by hand"
    );
    assert_eq!(
        words(&app, &[a, b]),
        [(None, AutopilotView::Cruise), (None, AutopilotView::Cruise)],
        "Cruise is given back"
    );
    assert_eq!(timers(&app, &[a, b]), [None, None], "the timers are not");
    let every = app.harness_state().every_row().expect("five sessions");
    assert_eq!(every.spoken_tail(land), "", "nothing timed: nothing to say");
    host.shutdown_and_join();
}

/// UNDER A DEFAULT OF OFF, FOLLOW SAYS THE HARNESS LETS GO (design §11.5.4):
/// three sessions on their own Cruise or Land, the default Off: Follow the
/// default says they follow `[harness] autopilot`, which is Off, and the
/// harness lets go of them (spoken `…, the default is Off`), and the press
/// turns them off. NEGATIVE CONTROL (run by hand, recorded in the landing
/// commit): Follow's words that never name the Off default fail here.
#[test]
fn follow_under_a_default_of_off_says_the_harness_lets_go() {
    use crate::harness_screen::EveryPress;
    let (mut app, host, ids) = five_sessions("[harness]\nautopilot = \"off\"\n");
    let [a, b, off, land, _] = ids;
    assert!(app.set_session_autopilot(
        a,
        Some(Autopilot::Cruise),
        crate::autopilot::Surface::Settings
    ));
    assert!(app.set_session_autopilot(
        b,
        Some(Autopilot::Cruise),
        crate::autopilot::Surface::Settings
    ));
    let every = app.harness_state().every_row().expect("five sessions");
    assert_eq!(
        every.description(EveryPress::Follow),
        "3 sessions follow [harness] autopilot, which is Off: the harness lets go of them; the \
         1 turned off stays off"
    );
    assert_eq!(
        every.spoken_tail(EveryPress::Follow),
        ", the default is Off"
    );
    assert_eq!(app.press_autopilot_every(EveryPress::Follow), 3);
    for (s, (word, view)) in [a, b, land].into_iter().zip(words(&app, &[a, b, land])) {
        assert_eq!((word, view), (None, AutopilotView::Off), "session {s}");
    }
    assert_eq!(app.session_autopilot(off).0, Some(Autopilot::Off));
    // CONTROL in the same App: under a default of Cruise the words are
    // the plain ones.
    app.harness_policy = crate::harness_policy_cell::HarnessPolicyCell::new(
        crate::app_config::HarnessPolicy::read("[harness.agy]\nenabled = false\n"),
    );
    assert!(app.set_session_autopilot(
        a,
        Some(Autopilot::Land),
        crate::autopilot::Surface::Settings
    ));
    let every = app.harness_state().every_row().expect("five sessions");
    assert_eq!(
        every.description(EveryPress::Follow),
        "1 session follows [harness] autopilot again; the 1 turned off stays off"
    );
    assert_eq!(every.spoken_tail(EveryPress::Follow), "");
    host.shutdown_and_join();
}

/// FOLLOW THE DEFAULT KEEPS A PINNED OFF (design §11.5.1): every session's
/// own Cruise or Land is cleared, so each follows `[harness] autopilot`
/// again; the session pinned Off keeps its word, and the one under its
/// agent's own switch off is skipped, its word kept. NEGATIVE CONTROL (run
/// by hand): a Follow that clears every word lifts the pinned Off, and fails
/// here.
#[test]
fn follow_the_default_keeps_a_pinned_off() {
    use crate::harness_screen::EveryPress;
    let (mut app, host, ids) = five_sessions("");
    let [a, _, off, land, agy] = ids;
    assert!(app.set_session_autopilot(
        a,
        Some(Autopilot::Cruise),
        crate::autopilot::Surface::Settings
    ));
    assert!(app.set_session_autopilot(
        agy,
        Some(Autopilot::Land),
        crate::autopilot::Surface::Settings
    ));
    assert_eq!(
        app.press_autopilot_every(EveryPress::Follow),
        2,
        "the two own words that carry"
    );
    assert_eq!(
        words(&app, &[a, off, land, agy]),
        [
            (None, AutopilotView::Cruise),
            (Some(Autopilot::Off), AutopilotView::Off),
            (None, AutopilotView::Cruise),
            (
                Some(Autopilot::Land),
                AutopilotView::AgentOff(aterm_phase::Program::Agy)
            ),
        ]
    );
    assert_eq!(
        app.press_autopilot_every(EveryPress::Follow),
        0,
        "a second press moves nothing"
    );
    host.shutdown_and_join();
}

/// A BULK LAND HANDS BACK AS THE ROW'S LAND DOES (design §11.5.3, item 3):
/// under a default of Land, Land every session leaves each session that
/// cruised by its own word with no word of its own, Land in force — the
/// very words the rows' own Land presses leave, one by one, on a twin App.
/// NEGATIVE CONTROL (run by hand): a bulk press that pins Land where the
/// row's press hands back fails the equality.
#[test]
fn a_bulk_land_hands_back_as_the_rows_land_does() {
    use crate::harness_screen::EveryPress;
    let setup = || {
        let (mut app, host, ids) = five_sessions("[harness]\nautopilot = \"land\"\n");
        let [a, b, ..] = ids;
        assert!(app.set_session_autopilot(
            a,
            Some(Autopilot::Cruise),
            crate::autopilot::Surface::Settings
        ));
        assert!(app.set_session_autopilot(
            b,
            Some(Autopilot::Cruise),
            crate::autopilot::Surface::Settings
        ));
        (app, host, ids)
    };
    let (mut bulk, bulk_host, ids) = setup();
    let [a, b, ..] = ids;
    assert_eq!(
        bulk.press_autopilot_every(EveryPress::Mode(Autopilot::Land)),
        2
    );
    let (mut rows, rows_host, _) = setup();
    for s in [a, b] {
        assert!(rows.press_page_mode(s, Autopilot::Land));
    }
    assert_eq!(words(&bulk, &ids), words(&rows, &ids), "as the rows' press");
    assert_eq!(
        words(&bulk, &[a, b]),
        [(None, AutopilotView::Land), (None, AutopilotView::Land)],
        "no word of their own, Land in force"
    );
    bulk_host.shutdown_and_join();
    rows_host.shutdown_and_join();
}

/// A PRESS FOR EVERY SESSION PUBLISHES THE PAGE ONCE (design §11.5.3, item
/// 6): with Settings ▸ Harness on screen, Land every session over five
/// cruising sessions moves all five and publishes the page once (the
/// publish's revision, counted at `publish_native_harness_state`), and the
/// page then lists each one landing. CONTROL: the rows' own Land, pressed on
/// each of the five in a loop, publishes five times.
#[test]
fn a_press_for_every_session_publishes_the_page_once() {
    use crate::harness_screen::EveryPress;
    let mut app = App::headless_for_test();
    let wid = WindowId(0);
    let mut ids = vec![0];
    for _ in 0..4 {
        let s = app.next_session_id;
        app.push_stub_tab(wid, crate::stub_session(s));
        ids.push(s);
    }
    for &s in &ids {
        run_claude(&app, s);
    }
    let host = idle_host();
    app.harness = Some(host.clone());
    assert!(app.open_harness_settings(wid, crate::harness_screen::HarnessFocus::Top));
    app.publish_native_harness_state();
    let revision = app.harness_screen.revision;
    assert_eq!(
        app.press_autopilot_every(EveryPress::Mode(Autopilot::Land)),
        5
    );
    assert_eq!(
        app.harness_screen.revision - revision,
        1,
        "one publish for five sessions"
    );
    let state = app.harness_state();
    assert!(
        state.sessions.iter().all(|s| s.view == AutopilotView::Land),
        "the page lists each one landing"
    );

    // CONTROL: the rows' own press, one session at a time.
    assert_eq!(
        app.press_autopilot_every(EveryPress::Mode(Autopilot::Cruise)),
        5
    );
    let revision = app.harness_screen.revision;
    for &s in &ids {
        assert!(app.press_page_mode(s, Autopilot::Land));
    }
    assert_eq!(
        app.harness_screen.revision - revision,
        5,
        "control: a press per session publishes per session"
    );
    host.shutdown_and_join();
}

/// THE ROW'S KEYS ARE READ BACK, AND NOTHING ELSE IS (design §11.5.2): each
/// press the row draws is minted under the session prefix and parsed back
/// to itself; an Off (no bulk Off), a cancel (no bulk cancel), a duration
/// the page does not offer, an alias of a mode, and a session's own key are
/// no press of the row. CONTROL: a session's own keys still parse as its
/// own.
#[test]
fn the_every_rows_keys_read_back_and_nothing_else_does() {
    use crate::harness_screen::{
        EveryPress, SESSION_ACTION_PREFIX, every_action, parse_every_action,
    };
    for press in EveryPress::ALL {
        let key = every_action(press);
        assert!(key.starts_with(SESSION_ACTION_PREFIX), "{key}");
        assert_eq!(parse_every_action(&key), Some(press), "{key}");
    }
    for key in [
        "harness/session/all/autopilot/off",
        "harness/session/all/land-cancel",
        "harness/session/all/land-in/30",
        "harness/session/all/autopilot/forever",
        "harness/session/all/timer",
        "harness/session/7/autopilot/land",
        "harness/session/allx/follow",
        "harness/all/follow",
    ] {
        assert_eq!(parse_every_action(key), None, "{key}");
    }
    assert_eq!(
        every_action(EveryPress::Mode(Autopilot::Off)),
        "harness/session/all/autopilot/off"
    );
    assert_eq!(
        parse_autopilot_action("harness/session/all/autopilot/land"),
        None,
        "a session's parser refuses `all`"
    );
    assert_eq!(
        parse_autopilot_action("harness/session/7/autopilot/land"),
        Some((7, Autopilot::Land)),
        "control"
    );
}

/// THE ROW SAYS WHAT IS THERE AND WHAT EACH PRESS MOVES (design §11.5.1),
/// from the five sessions' snapshot: `5 sessions: 2 cruising (1 timed), 1
/// landing, 2 off`, and each press's count by the press's own rule. Under
/// the master switch off the row says why it is inert. With one session
/// there is no row.
#[test]
fn the_every_row_counts_by_the_presses_own_rule() {
    use crate::harness_screen::EveryPress;
    let (mut app, host, ids) = five_sessions("");
    let [a, ..] = ids;
    let now = std::time::Instant::now();
    set_fake_clocks(Some((now, 1_792_177_230_000)));
    assert!(app.choose_autopilot_timer(
        a,
        TimerChoice::In(60),
        crate::autopilot::Surface::Settings
    ));
    set_fake_clocks(None);
    let every = app.harness_state().every_row().expect("five sessions");
    assert_eq!(
        every.line(),
        "5 sessions: 2 cruising (1 timed), 1 landing, 2 off"
    );
    assert_eq!(
        every.description(EveryPress::Mode(Autopilot::Land)),
        "Land 2 sessions and cancel 1 timer; the 2 off stay off"
    );
    assert_eq!(
        every.description(EveryPress::Mode(Autopilot::Cruise)),
        "Cruise 2 sessions: 1 landing, 1 timed; the 2 off stay off"
    );
    assert_eq!(
        every.description(EveryPress::Timer(TimerChoice::In(60))),
        "Time 2 sessions to land together; the 2 off stay off"
    );
    assert_eq!(
        every.description(EveryPress::Follow),
        "2 sessions follow [harness] autopilot again, 1 timer cancelled; the 1 turned off stays off"
    );
    // Each count is what the press then moves.
    for press in [
        EveryPress::Mode(Autopilot::Cruise),
        EveryPress::Follow,
        EveryPress::Mode(Autopilot::Land),
        EveryPress::Timer(TimerChoice::In(60)),
    ] {
        let counted = app.harness_state().every_row().expect("row").moved(press);
        assert_eq!(app.press_autopilot_every(press), counted, "{press:?}");
    }
    app.harness_policy = crate::harness_policy_cell::HarnessPolicyCell::new(
        crate::app_config::HarnessPolicy::read("[harness]\nenabled = false\n"),
    );
    let every = app.harness_state().every_row().expect("still five");
    assert_eq!(every.steered, 0);
    assert_eq!(
        every.line(),
        "5 sessions \u{b7} harness off in Settings",
        "the rows' own words"
    );
    for press in EveryPress::ALL {
        assert_eq!(app.press_autopilot_every(press), 0, "{press:?}: inert");
    }
    host.shutdown_and_join();

    let mut one = App::headless_for_test();
    run_claude(&one, 0);
    let one_host = idle_host();
    one.harness = Some(one_host.clone());
    assert_eq!(one.harness_state().sessions.len(), 1);
    assert_eq!(one.harness_state().every_row(), None, "one session: no row");
    one_host.shutdown_and_join();
}

/// WHERE IT STEERS NONE, THE ROW SAYS WHY IN THE ROWS' OWN WORDS: every
/// session under its agent's own switch off reads that switch by name, as
/// each row does (`3 sessions · Supervise Claude Code sessions is off`).
#[test]
fn the_inert_row_names_the_agents_own_switch() {
    let mut app = App::headless_for_test();
    let wid = WindowId(0);
    for _ in 0..2 {
        let s = app.next_session_id;
        app.push_stub_tab(wid, crate::stub_session(s));
    }
    for s in 0..3 {
        run_claude(&app, s);
    }
    let host = idle_host();
    app.harness = Some(host.clone());
    app.harness_policy = crate::harness_policy_cell::HarnessPolicyCell::new(
        crate::app_config::HarnessPolicy::read("[harness.claude]\nenabled = false\n"),
    );
    let every = app.harness_state().every_row().expect("three sessions");
    assert_eq!(every.steered, 0);
    assert_eq!(
        every.line(),
        "3 sessions \u{b7} Supervise Claude Code sessions is off"
    );
    host.shutdown_and_join();
}

/// Settings ▸ Harness drawn as the window's next frame draws it, and that
/// frame shown, so a pointer motion or press reads it.
fn present_settings(app: &mut App, wid: WindowId) -> crate::tab_model::ViewId {
    let (_, view) = app.active_native_view(wid).expect("Settings in front");
    assert!(app.prepare_native_input_scratch(wid));
    app.windows
        .get_mut(&wid)
        .expect("window")
        .leaf_render_cache
        .get_mut(&view)
        .expect("drawn")
        .native
        .as_mut()
        .expect("a native frame")
        .presented = true;
    view
}

/// The window pixel at the centre of control `key` in the shown frame.
fn window_point(app: &App, wid: WindowId, view: crate::tab_model::ViewId, key: &str) -> (f64, f64) {
    let artifact = app
        .retained_native_leaf_artifact(wid, view, true)
        .expect("a shown frame");
    let hit = artifact
        .compiled
        .hits
        .iter()
        .find(|hit| hit.key.as_str() == key)
        .unwrap_or_else(|| panic!("{key} is drawn"));
    let scale = artifact.scale;
    (
        artifact.device_x as f64 + f64::from(hit.rect.x + hit.rect.width * 0.5) * scale,
        artifact.device_y as f64 + f64::from(hit.rect.y + hit.rect.height * 0.5) * scale,
    )
}

/// The control at window pixel `px` in the shown frame, by key.
fn key_at(app: &App, wid: WindowId, (x, y): (f64, f64)) -> Option<String> {
    let (artifact, x, y) = app.retained_native_leaf_at_pointer(wid, x, y)?;
    artifact
        .compiled
        .hit_test(x, y)
        .map(|hit| hit.key.as_str().to_string())
}

/// A person's click where the pointer is, as macOS delivers one: a motion
/// event at the press point (no move) ahead of the press, and again ahead
/// of the release.
fn click_at(app: &mut App, wid: WindowId, (x, y): (f64, f64)) {
    use aterm_window::event::{ElementState, MouseButton};
    for state in [ElementState::Pressed, ElementState::Released] {
        app.on_cursor_moved(wid, x, y);
        app.on_mouse_input(wid, state, MouseButton::Left);
    }
}

/// A PRESS THE LAYOUT MOVED UNDER A STILL POINTER CHOOSES NOTHING (design
/// §11.5.7, lifecycle-1): three Claude Code sessions cruising on Settings ▸
/// Harness, the pointer resting one row's pitch above the bulk Land; the
/// middle session closes, and the page, republished, lifts the last row
/// into the middle one's place and the `Every session` row under it, so its
/// bulk Land now stands under the still pointer. A click there lands
/// nothing — no session moves — and lights the bulk Land; the next click,
/// the pointer now resting on it, lands every session. A scroll the person
/// asks for forgets the rest: what then comes under the pointer is theirs
/// to press. NEGATIVE CONTROL (run by hand, recorded in the landing
/// commit): the press path without its look (`NativePointerRest::looked_at`
/// answering yes) lands both sessions on the first click, and fails here.
#[test]
fn a_press_the_layout_moved_under_a_still_pointer_chooses_nothing() {
    let mut app = App::headless_for_test();
    let wid = WindowId(0);
    let mut ids = vec![0];
    for _ in 0..2 {
        let s = app.next_session_id;
        app.push_stub_tab(wid, crate::stub_session(s));
        ids.push(s);
    }
    for &s in &ids {
        run_claude(&app, s);
    }
    let [first, middle, last]: [u64; 3] = ids.try_into().expect("three");
    let host = idle_host();
    app.harness = Some(host.clone());
    // A window that shows SESSIONS whole: the page is laid out in its grid.
    {
        let ws = app.windows.get_mut(&wid).expect("window");
        ws.cols = 160;
        ws.rows = 90;
    }
    assert!(app.open_harness_settings(wid, crate::harness_screen::HarnessFocus::Top));
    app.publish_native_harness_state();
    let view = present_settings(&mut app, wid);
    let bulk = "harness/session/all/autopilot/land";
    let pitch = window_point(
        &app,
        wid,
        view,
        &format!("harness/session/{last}/autopilot/land"),
    )
    .1 - window_point(
        &app,
        wid,
        view,
        &format!("harness/session/{middle}/autopilot/land"),
    )
    .1;
    let (x, y) = window_point(&app, wid, view, bulk);
    let rest = (x, y - pitch);
    assert_ne!(key_at(&app, wid, rest).as_deref(), Some(bulk));
    app.on_cursor_moved(wid, rest.0, rest.1);

    assert!(app.close_session(wid, middle) || app.pool.get(middle).is_none());
    app.publish_native_harness_state();
    assert_eq!(app.harness_state().sessions.len(), 2);
    present_settings(&mut app, wid);
    assert_eq!(
        key_at(&app, wid, rest).as_deref(),
        Some(bulk),
        "the bulk Land moved under the still pointer"
    );
    let cruising = words(&app, &[first, last]);
    assert_eq!(
        cruising,
        [(None, AutopilotView::Cruise), (None, AutopilotView::Cruise)]
    );
    click_at(&mut app, wid, rest);
    assert_eq!(
        words(&app, &[first, last]),
        cruising,
        "the click landed none"
    );
    assert_eq!(
        app.native_runtime
            .view_state(view)
            .and_then(|state| state.common().hovered.as_ref())
            .map(|key| key.as_str()),
        Some(bulk),
        "the bulk Land is lit"
    );
    click_at(&mut app, wid, rest);
    assert_eq!(
        words(&app, &[first, last]),
        [
            (Some(Autopilot::Land), AutopilotView::Land),
            (Some(Autopilot::Land), AutopilotView::Land),
        ],
        "the next click lands every session"
    );

    // A scroll the person asks for forgets where the pointer rested.
    assert!(app.windows[&wid].native_rest.is_some());
    let _ = app.dispatch_native_event(wid, AppEvent::ScrollLines(1));
    assert!(app.windows[&wid].native_rest.is_none());
    host.shutdown_and_join();
}

/// The shown frame's controls whose keys start with `prefix`, by key, with
/// their rects in window pixels.
fn rects_of(
    app: &App,
    wid: WindowId,
    view: crate::tab_model::ViewId,
    prefix: &str,
) -> Vec<(String, [f64; 4])> {
    let artifact = app
        .retained_native_leaf_artifact(wid, view, true)
        .expect("a shown frame");
    let (scale, dx, dy) = (
        artifact.scale,
        artifact.device_x as f64,
        artifact.device_y as f64,
    );
    artifact
        .compiled
        .hits
        .iter()
        .filter(|hit| hit.key.as_str().starts_with(prefix))
        .map(|hit| {
            let r = hit.rect;
            (
                hit.key.as_str().to_string(),
                [
                    dx + f64::from(r.x) * scale,
                    dy + f64::from(r.y) * scale,
                    dx + f64::from(r.x + r.width) * scale,
                    dy + f64::from(r.y + r.height) * scale,
                ],
            )
        })
        .collect()
}

/// Session `s` running Claude Code, or a plain shell.
fn run_program(app: &App, s: u64, program: Option<&str>) {
    let ctx = app.pool.get(s).unwrap().ctx.clone();
    let mut tl = ctx.timeline.lock().unwrap();
    tl.note_foreground_group(7);
    tl.set_program(7, program.map(Into::into));
}

/// NOR A ROW'S PRESS ONE MEANT FOR THE BULK ROW (the lab, 2026-10-19: a row's
/// Cruise that the page moved under a pointer resting on the bulk Cruise
/// took the click, and a session its person had turned Off cruised): two
/// Claude Code sessions cruising, the pointer resting on a bulk button; two
/// more tabs' shells become Claude Code sessions, which their person turned
/// Off, and the page, republished, puts their rows where the `Every session`
/// row was, so one of their buttons stands under the still pointer (where,
/// the frames say: the bulk row lies beside its words or under them). A
/// click there turns nothing on — the Off sessions stay Off, the others
/// cruise — and lights that button; the next click, the pointer now resting
/// on it, presses it. NEGATIVE CONTROL (run by hand, recorded in the landing
/// commit): the look asked only of the control pressed
/// (`NativePointerRest::looked_at` without what the pointer saw) presses
/// the row's button on the first click, and fails here.
#[test]
fn a_row_the_layout_moved_under_a_pointer_on_the_bulk_row_waits_for_a_look() {
    let mut app = App::headless_for_test();
    let wid = WindowId(0);
    let mut ids = vec![0];
    for _ in 0..3 {
        let s = app.next_session_id;
        app.push_stub_tab(wid, crate::stub_session(s));
        ids.push(s);
    }
    let ids: [u64; 4] = ids.try_into().expect("four");
    let host = idle_host();
    app.harness = Some(host.clone());
    {
        let ws = app.windows.get_mut(&wid).expect("window");
        ws.cols = 160;
        ws.rows = 90;
    }
    for &s in &ids {
        run_program(&app, s, Some("claude"));
    }
    for &s in &ids[2..] {
        assert!(app.set_session_autopilot(
            s,
            Some(Autopilot::Off),
            crate::autopilot::Surface::Settings
        ));
    }
    assert!(app.open_harness_settings(wid, crate::harness_screen::HarnessFocus::Top));
    let shown = |app: &mut App, claude: bool| {
        for &s in &ids[2..] {
            run_program(app, s, claude.then_some("claude"));
        }
        app.publish_native_harness_state();
        assert_eq!(
            app.harness_state().sessions.len(),
            if claude { 4 } else { 2 }
        );
        present_settings(app, wid)
    };
    // Where a bulk button before meets a new row's button after.
    let view = shown(&mut app, true);
    let rows: Vec<_> = ids[2..]
        .iter()
        .flat_map(|s| rects_of(&app, wid, view, &format!("harness/session/{s}/")))
        .collect();
    let view = shown(&mut app, false);
    let bulk = rects_of(&app, wid, view, "harness/session/all/");
    let (rest, row) = bulk
        .iter()
        .flat_map(|(_, b)| rows.iter().map(move |(key, r)| (b, key, r)))
        .find_map(|(b, key, r)| {
            let (x0, y0, x1, y1) = (
                b[0].max(r[0]),
                b[1].max(r[1]),
                b[2].min(r[2]),
                b[3].min(r[3]),
            );
            (x1 - x0 >= 2.0 && y1 - y0 >= 2.0)
                .then(|| (((x0 + x1) / 2.0, (y0 + y1) / 2.0), key.clone()))
        })
        .expect("a new row's button comes where a bulk one was");
    let seen = key_at(&app, wid, rest).expect("a bulk button under the pointer");
    assert!(seen.starts_with("harness/session/all/"), "{seen}");
    app.on_cursor_moved(wid, rest.0, rest.1);

    let view = shown(&mut app, true);
    assert_eq!(
        key_at(&app, wid, rest).as_deref(),
        Some(row.as_str()),
        "a new row's button moved under the still pointer"
    );
    let before = words(&app, &ids);
    let off = (Some(Autopilot::Off), AutopilotView::Off);
    let cruising = (None, AutopilotView::Cruise);
    assert_eq!(before, [cruising, cruising, off, off]);
    click_at(&mut app, wid, rest);
    assert_eq!(words(&app, &ids), before, "the click turned nothing on");
    assert_eq!(
        app.native_runtime
            .view_state(view)
            .and_then(|state| state.common().hovered.as_ref())
            .map(|key| key.as_str()),
        Some(row.as_str()),
        "the row's button is lit"
    );
    click_at(&mut app, wid, rest);
    let pressed: u64 = row
        .split('/')
        .nth(2)
        .and_then(|s| s.parse().ok())
        .expect("a session's row");
    let after = words(&app, &ids);
    for (i, &s) in ids.iter().enumerate() {
        if s == pressed {
            assert_ne!(after[i], before[i], "the next click presses {row}");
        } else {
            assert_eq!(after[i], before[i], "only {row}'s session moves");
        }
    }
    host.shutdown_and_join();
}

/// A SESSIONS ROW'S OWN BUTTONS WAIT FOR A LOOK TOO (the post-landing check's
/// owner-eye-4, design §11.5.7): three Claude Code sessions cruising on
/// Settings ▸ Harness, the first under a timer, so its row says `lands at
/// …`. The pointer comes to rest where, once that line goes (the timer's
/// fire, or another surface cancelling it), the rows under it come up 22 pt
/// and one of their buttons stands — a point that showed some other thing
/// before. The line goes; a click there presses nothing (no session's word,
/// no disclosure, no tab moves) and lights that button; the next click, the
/// pointer now resting on it, presses it. NEGATIVE CONTROL: the row's buttons
/// without `Control::look_first` (main's) take the first click.
#[test]
fn a_rows_button_a_state_line_moved_under_a_still_pointer_waits_for_a_look() {
    use crate::native_app::HarnessTimerRequest;
    let mut app = App::headless_for_test();
    let wid = WindowId(0);
    let mut ids = vec![0];
    for _ in 0..2 {
        let s = app.next_session_id;
        app.push_stub_tab(wid, crate::stub_session(s));
        ids.push(s);
    }
    for &s in &ids {
        run_claude(&app, s);
    }
    let host = idle_host();
    app.harness = Some(host.clone());
    {
        let ws = app.windows.get_mut(&wid).expect("window");
        ws.cols = 160;
        ws.rows = 90;
    }
    assert!(app.open_harness_settings(wid, crate::harness_screen::HarnessFocus::Top));
    let first = ids[0];
    let timed = |app: &mut App, armed: bool| {
        app.execute_harness_timer(HarnessTimerRequest {
            session: first,
            choice: if armed {
                TimerChoice::In(60)
            } else {
                TimerChoice::Cancel
            },
        });
        app.publish_native_harness_state();
        let view = present_settings(app, wid);
        let line = format!("harness/session/{first}/state");
        let artifact = app
            .retained_native_leaf_artifact(wid, view, true)
            .expect("a shown frame");
        assert_eq!(
            artifact
                .compiled
                .paint
                .iter()
                .any(|node| node.key.as_str() == line),
            armed,
            "the first row's `lands at` line is drawn while its timer is armed"
        );
        view
    };
    // Where a row's button stands once the line goes, over what the page
    // showed there with it.
    let view = timed(&mut app, false);
    let after = rects_of(&app, wid, view, "harness/session/");
    let view = timed(&mut app, true);
    let before = rects_of(&app, wid, view, "");
    let under = |rects: &[(String, [f64; 4])], (x, y): (f64, f64)| {
        rects
            .iter()
            .find(|(_, r)| x >= r[0] && x < r[2] && y >= r[1] && y < r[3])
            .map(|(key, _)| key.clone())
    };
    let (rest, row) = after
        .iter()
        // A button whose press shows (the sessions cruise already by their
        // default, which a Cruise press hands back to, moving nothing).
        .filter(|(key, _)| {
            !key.starts_with("harness/session/all/")
                && !key.contains(&format!("/{first}/"))
                && !key.ends_with("/autopilot/cruise")
        })
        .find_map(|(key, r)| {
            let mut y = r[1] + 2.0;
            while y < r[3] - 2.0 {
                let p = ((r[0] + r[2]) / 2.0, y);
                if under(&before, p).as_deref() != Some(key.as_str()) {
                    return Some((p, key.clone()));
                }
                y += 2.0;
            }
            None
        })
        .expect("a row's button comes where the page showed another thing");
    app.on_cursor_moved(wid, rest.0, rest.1);
    let view = timed(&mut app, false);
    assert_eq!(
        key_at(&app, wid, rest).as_deref(),
        Some(row.as_str()),
        "{row} moved under the still pointer"
    );
    let state = |app: &App| {
        let open = match app.native_runtime.view_state(view) {
            Some(crate::native_app::AppViewState::Settings(state)) => state.harness_timer_open,
            _ => None,
        };
        (
            words(app, &ids),
            open,
            app.active_native_view(wid).map(|(_, view)| view),
        )
    };
    let still = state(&app);
    click_at(&mut app, wid, rest);
    assert_eq!(state(&app), still, "the click pressed nothing");
    assert_eq!(
        app.native_runtime
            .view_state(view)
            .and_then(|state| state.common().hovered.as_ref())
            .map(|key| key.as_str()),
        Some(row.as_str()),
        "{row} is lit"
    );
    click_at(&mut app, wid, rest);
    assert_ne!(state(&app), still, "the next click presses {row}");
    host.shutdown_and_join();
}

/// THE EVERY ROW READS EACH SESSION'S CHIP (design §12.4.3, item 3, slice
/// 4c): on the App, a session under Land is counted by the face its chip
/// wears, from the glass the loop's facts set. Of five sessions, Land every
/// session lands the two cruising ones beside the one under Land by its own
/// word; the loop tells one landed and one a person's own exchange. The row
/// says `1 landing, 1 landed, 1 quiet`, the Cruise press says the same of
/// what it moves, and moves all three (read by those parts: the rest of
/// each line is other features' words). NEGATIVE CONTROL (run by hand,
/// recorded in the slice's commit): counting modes says `3 landing`.
#[test]
fn the_every_row_reads_each_sessions_chip_face() {
    use crate::claude_lights::ChipFace;
    use crate::harness_screen::EveryPress;
    use aterm_agent::harness::acts::TaskFace;
    let (mut app, host, ids) = five_sessions("");
    let [a, b, _off, land, _agy] = ids;
    assert_eq!(
        app.press_autopilot_every(EveryPress::Mode(Autopilot::Land)),
        2,
        "the two that cruise"
    );
    let sid = |app: &App, s: u64| app.pool.get(s).unwrap().ctx.self_id.as_str().to_string();
    let (landed, quiet) = (sid(&app, land), sid(&app, b));
    app.apply_harness_fact(&landed, crate::HarnessFact::Task(Some(TaskFace::Landed)));
    app.apply_harness_fact(&quiet, crate::HarnessFact::Task(Some(TaskFace::Theirs)));
    let state = app.harness_state();
    let face = |s: u64| {
        state
            .sessions
            .iter()
            .find(|row| row.local_id == s)
            .map(|row| ChipFace::of(row.view, &row.glass))
    };
    assert_eq!(
        [face(a), face(b), face(land)],
        [
            Some(ChipFace::Land),
            Some(ChipFace::Theirs),
            Some(ChipFace::Landed)
        ],
        "the page holds each chip's face"
    );
    let every = state.every_row().expect("five sessions");
    assert_eq!((every.landing, every.landed, every.quiet), (1, 1, 1));
    let cruise = EveryPress::Mode(Autopilot::Cruise);
    let (line, description) = (every.line(), every.description(cruise));
    for part in ["1 landing", "1 landed", "1 quiet"] {
        assert!(super::landings::says(&line, part), "{part}: {line}");
        assert!(
            super::landings::says(&description, part),
            "{part}: {description}"
        );
    }
    assert_eq!(app.press_autopilot_every(cruise), 3, "all three it names");
    host.shutdown_and_join();
}
