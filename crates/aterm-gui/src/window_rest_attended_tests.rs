// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! An unattended window through the App: what its deck schedules, the frames the lapse and the
//! next touch ask for, and who is always attended. Counts on a clock the test steps.

use super::ATTENTION;
use crate::input::{InputEvent, InputOutcome, Source, WindowResize};
use crate::stats_live::{DeckLive, FRAME, STALE_PERIODS};
use crate::vitals_host::{Audience, BACKGROUND_PERIOD, FOCUSED_PERIOD};
use aterm_window::dpi::PhysicalSize;
use aterm_window::event::{InputOrigin, WindowEvent};
use std::time::{Duration, Instant};

/// 10:42:10 on the deck fixtures' day, unix ms.
const OPEN: i64 = 1_792_000_800_000 + 42 * 60_000 + 10_000;

/// A window on screen, focused, drawing a live deck over a reading landed at `t0` and read on
/// the focused cadence, touched at `t0`.
struct Rig {
    app: crate::App,
    wid: crate::WindowId,
    t0: Instant,
    _attached: crate::stats_deck::tests::Attached,
    _reader: Box<dyn std::any::Any>,
}

impl Rig {
    fn new() -> Self {
        let attached = crate::stats_deck::tests::attach_stats();
        let t0 = Instant::now();
        let (mut app, wid) = crate::stats_live::tests::live_app(t0);
        app.headless = false;
        let reader = app.vitals.attach_test_reader();
        app.service_vitals(t0);
        app.vitals.landed_at = Some(t0);
        app.vitals.sampled_at = Some(t0);
        app.note_rest_event(wid, &crate::window_rest::tests::key(), t0);
        Self {
            app,
            wid,
            t0,
            _attached: attached,
            _reader: Box::new(reader),
        }
    }

    fn frames_asked(&self) -> u32 {
        self.app.windows[&self.wid].redraw_requests_for_test.get()
    }

    fn moves(&self) -> bool {
        self.app
            .live_facts(self.wid)
            .expect("the deck draws")
            .motion
    }

    /// The deadlines the deck's motion arms at `now`: the live edge's next frame (a deadline
    /// before the grey's moment) and the deck clock's. The fixture's needle owes no sweep;
    /// `an_unattended_windows_sweeping_needle_runs_no_display_link` holds an unattended
    /// window's clock still under one.
    fn motion_deadlines(&self, now: Instant) -> usize {
        let facts = self.app.live_facts(self.wid).expect("the deck draws");
        let grey = self.app.vitals.sampled_at.expect("a reading") + facts.stale_after;
        let edge = self
            .app
            .stats_live_deadline(now)
            .is_some_and(|at| at < grey);
        usize::from(edge) + usize::from(self.app.deck_clock_deadline(now).is_some())
    }
}

/// Past its attention a focused window reads as one in the background: one frame is asked at
/// the lapse, the live edge and the deck clock arm nothing, only the grey's moment stays
/// scheduled, and the readings are asked a background period apart. Each reading that lands
/// asks one frame, which draws the deck still on that reading, and a turn between two asks
/// neither a frame nor a reading. Output in between changes none of it. The next touch asks a
/// reading at once, and the focused cadence follows it.
///
/// Negative control, run once by hand and restored by copy: `vitals_host::attends_readings`
/// counting a window that does not rest whether or not it is attended (without its
/// `ws.rest.attended()` term) fails "unattended: the background's audience", left `Shown`, right
/// `Background`.
#[test]
fn an_unattended_windows_deck_is_still_and_reads_on_the_background_cadence() {
    let mut rig = Rig::new();
    let (wid, t0) = (rig.wid, rig.t0);
    assert!(rig.moves());
    assert!(
        rig.app
            .stats_live_deadline(t0)
            .is_some_and(|at| at > t0 && at <= t0 + FRAME),
        "the live edge's next frame"
    );
    assert_eq!(
        rig.app.vitals.next_due,
        Some(t0 + FOCUSED_PERIOD),
        "attended: the focused cadence"
    );
    let frames = rig.frames_asked();

    let output = t0 + Duration::from_secs(30);
    let mut ws = rig.app.windows.get_mut(&wid).expect("the window");
    assert!(!ws.rest.stir(output), "output stirs an awake window");
    drop(ws);
    rig.app
        .settle_rest(t0 + ATTENTION - Duration::from_millis(1));
    assert!(rig.app.windows[&wid].rest.attended());
    assert!(rig.moves(), "a millisecond short of its attention");
    assert_eq!(rig.app.vitals_audience(), Audience::Shown);
    assert_eq!(rig.frames_asked(), frames);

    let lapse = t0 + ATTENTION;
    rig.app.settle_rest(lapse);
    assert!(
        !rig.app.windows[&wid].rest.attended(),
        "output 10 s ago attended nothing"
    );
    assert_eq!(rig.frames_asked(), frames + 1, "one frame for the lapse");
    assert_eq!(
        rig.app.vitals_audience(),
        Audience::Background,
        "unattended: the background's audience"
    );
    let still = rig.app.live_facts(wid).expect("the deck still draws");
    assert!(!still.motion, "{still:?}");
    assert_eq!(still.live(t0, OPEN), DeckLive::STILL);
    assert_eq!(
        still.stale_after,
        BACKGROUND_PERIOD * STALE_PERIODS,
        "a reading speaks for the background's cadence"
    );
    assert_eq!(
        rig.app.stats_live_deadline(t0),
        Some(t0 + still.stale_after),
        "only the grey is scheduled"
    );
    assert_eq!(rig.app.deck_clock_deadline(t0), None);
    rig.app.service_vitals(lapse);
    assert_eq!(
        rig.app.timed_reading_asked(),
        Some(Some(lapse)),
        "the lapse's turn asks the reading the focused cadence owed"
    );
    let mut asked = lapse;
    for k in 1..=4_u32 {
        let due = asked + BACKGROUND_PERIOD;
        assert_eq!(
            rig.app.vitals.next_due,
            Some(due),
            "reading {k}: armed a background period on"
        );
        // A turn a focused period on: no reading is due, and none landed.
        let early = asked + FOCUSED_PERIOD;
        rig.app.settle_rest(early);
        rig.app.service_vitals(early);
        assert_eq!(
            rig.app.timed_reading_asked(),
            Some(Some(asked)),
            "reading {k}: a focused period on, nothing is asked"
        );
        assert_eq!(rig.frames_asked(), frames + k, "reading {k}: nor drawn");
        rig.app.settle_rest(due);
        rig.app.service_vitals(due);
        assert_eq!(
            rig.app.timed_reading_asked(),
            Some(Some(due)),
            "reading {k}: asked a background period on"
        );
        // The probe thread hands the reading over, and its `Wake::Vitals` turn settles the
        // rest and serves the readings, as every turn does.
        let at = due + FOCUSED_PERIOD / 4;
        rig.app.vitals.land_for_test(u64::from(k), at);
        rig.app.settle_rest(at);
        rig.app.service_vitals(at);
        assert_eq!(
            rig.app.vitals.landed_at,
            Some(at),
            "reading {k}: the turn adopted it"
        );
        assert_eq!(
            rig.frames_asked(),
            frames + 1 + k,
            "reading {k}: one frame draws the deck on it"
        );
        let still = rig.app.live_facts(wid).expect("the deck still draws");
        assert!(!still.motion, "reading {k}: {still:?}");
        assert_eq!(
            rig.motion_deadlines(at),
            0,
            "reading {k}: no motion deadline"
        );
        asked = due;
    }

    // Two focused periods after the last ask, so the focused cadence owes a reading now.
    let touch = asked + FOCUSED_PERIOD * 2;
    rig.app
        .note_rest_event(wid, &crate::window_rest::tests::key(), touch);
    assert!(rig.app.windows[&wid].rest.attended(), "the key attends it");
    assert_eq!(rig.app.vitals_audience(), Audience::Shown);
    assert_eq!(
        rig.app.vitals.next_due,
        Some(touch),
        "the touch asks a reading at once"
    );
    rig.app.service_vitals(touch);
    assert_eq!(rig.app.timed_reading_asked(), Some(Some(touch)));
    assert_eq!(
        rig.app.vitals.next_due,
        Some(touch + FOCUSED_PERIOD),
        "the focused cadence follows"
    );
}

/// The lapse asks a frame only of a window whose deck it stops: a focused window on screen that
/// draws a moving deck. An unfocused window's deck, an occluded window's and a window's with the
/// bars off are still before the lapse and after it, and their lapse asks none.
///
/// Negative control, run once by hand and restored by copy: `settle_rest` asking a frame of
/// every window whose attention it changed fails "an unfocused window's lapse asks no frame".
#[test]
fn the_lapse_asks_a_frame_only_of_a_window_whose_deck_it_stops() {
    let lapse_of = |rig: &mut Rig| {
        let frames = rig.frames_asked();
        rig.app.settle_rest(rig.t0 + ATTENTION);
        assert!(!rig.app.windows[&rig.wid].rest.attended());
        rig.frames_asked() - frames
    };
    let mut moving = Rig::new();
    assert!(
        moving.moves(),
        "CONTROL: a focused window on screen moves its deck"
    );
    assert_eq!(lapse_of(&mut moving), 1, "the lapse stops it: one frame");

    let mut unfocused = Rig::new();
    let wid = unfocused.wid;
    unfocused
        .app
        .windows
        .get_mut(&wid)
        .expect("the window")
        .focused = false;
    assert_eq!(
        lapse_of(&mut unfocused),
        0,
        "an unfocused window's lapse asks no frame"
    );

    let mut covered = Rig::new();
    let wid = covered.wid;
    covered
        .app
        .windows
        .get_mut(&wid)
        .expect("the window")
        .occluded = true;
    assert_eq!(
        lapse_of(&mut covered),
        0,
        "an occluded window's lapse asks no frame"
    );

    let mut bare = Rig::new();
    bare.app
        .config
        .sys
        .get_or_insert_with(Default::default)
        .stats_bars = Some("off".into());
    assert!(bare.app.live_facts(bare.wid).is_none(), "CONTROL: no deck");
    assert_eq!(
        lapse_of(&mut bare),
        0,
        "a window that draws no deck asks no frame for its lapse"
    );
}

/// The deck clock's port, ticked by hand: a window that has a display link; it reaches none.
fn inert_link(_: Option<&aterm_window::window::Window>, _: bool) -> bool {
    true
}

/// An unattended window's deck clock wants no tick while its needle owes a sweep, as an
/// occluded or unfocused one's wants none: the lapse pauses the running link and arms no
/// deadline, the touch that ends the lapse runs nothing on its own, and the sweep of the first
/// reading that lands after the touch runs the link again. This deck's needle sweeps from a
/// calm reading to a busy one landed a second later, on a clock that starts a whole attention
/// after the window's last touch, so the second it sweeps in is past that attention however
/// long the window took to open.
///
/// Negative control, run once by hand and restored by copy: `Rest::deck_moves` without its
/// `self.attended` term fails "the lapse pauses the link".
#[test]
fn an_unattended_windows_sweeping_needle_runs_no_display_link() {
    use crate::stats_live::tests::{busier, fit_deck, live_app};
    let _attached = crate::stats_deck::tests::attach_stats();
    let touched = Instant::now();
    let t0 = touched + ATTENTION;
    let (mut app, wid) = live_app(t0);
    app.headless = false;
    app.note_rest_event(wid, &crate::window_rest::tests::key(), touched);
    app.vitals.landed_period = Some(Duration::from_secs(3600));
    let mut ws = app.windows.get_mut(&wid).expect("the window");
    ws.cols = 160;
    ws.scale = 2.0;
    ws.deck_clock.port = inert_link;
    drop(ws);
    fit_deck(&mut app, wid);
    let _ = app.stats_deck_fp(wid);
    assert!(
        app.windows[&wid].deck_needle_px.get() > 0.0,
        "CONTROL: the deck draws a dial"
    );
    app.retarget_needle(t0);
    let _ = app.stats_deck_fp(wid);
    let t1 = t0 + Duration::from_secs(1);
    let n = busier(&mut app, 0.95, t1);
    assert!(
        (n.tip.x0 - n.target().0).abs() > 10.0,
        "CONTROL: the needle sweeps"
    );
    app.sync_deck_clocks(t1);
    assert!(
        app.windows[&wid].deck_clock.link_running(),
        "CONTROL: an attended window's sweep runs the link"
    );

    app.settle_rest(t1);
    assert!(
        !app.windows[&wid].rest.attended(),
        "untouched for a whole attention"
    );
    app.sync_deck_clocks(t1);
    assert!(
        !app.windows[&wid].deck_clock.link_running(),
        "the lapse pauses the link"
    );
    assert_eq!(app.deck_clock_deadline(t1), None, "and arms no deadline");

    app.note_rest_event(wid, &crate::window_rest::tests::key(), t1);
    assert!(app.windows[&wid].rest.attended(), "the key attends it");
    app.sync_deck_clocks(t1);
    assert!(
        !app.windows[&wid].deck_clock.link_running(),
        "the touch alone runs nothing"
    );
    let t2 = t1 + Duration::from_secs(1);
    let _ = busier(&mut app, 0.10, t2);
    assert!(
        app.deck_clock_deadline(t2).is_some(),
        "CONTROL: the reading after the touch sweeps the needle"
    );
    app.sync_deck_clocks(t2);
    assert!(
        app.windows[&wid].deck_clock.link_running(),
        "that sweep runs the link again"
    );
}

/// The first key after a lapse: on its own turn it asks no frame and arms no motion deadline,
/// where the focus coming back to a window arms the live edge at once; the deck moves from the
/// first reading that lands after the key. The reading the touch itself asks is asserted by
/// `an_unattended_windows_deck_is_still_and_reads_on_the_background_cadence` (a key) and
/// `the_focus_that_ends_a_lapse_asks_a_reading_at_once` (the focus).
#[test]
fn the_first_key_after_a_lapse_arms_no_deck_motion_on_its_own_turn() {
    let mut rig = Rig::new();
    let lapse = rig.t0 + ATTENTION;
    rig.app.vitals.landed_at = Some(lapse);
    rig.app.vitals.sampled_at = rig.app.vitals.landed_at;
    rig.app.settle_rest(lapse);
    assert!(!rig.moves());
    let frames = rig.frames_asked();

    // A reading landed a moment before the key, so a deck that moved would arm its live edge.
    let key_at = lapse + Duration::from_secs(5);
    rig.app.vitals.landed_at = Some(key_at - Duration::from_millis(200));
    rig.app.vitals.sampled_at = rig.app.vitals.landed_at;
    rig.app
        .note_rest_event(rig.wid, &crate::window_rest::tests::key(), key_at);
    assert!(rig.app.windows[&rig.wid].rest.attended(), "at once");
    assert_eq!(rig.frames_asked(), frames, "the touch asks no frame");
    assert_eq!(rig.motion_deadlines(key_at), 0, "nor any deck motion");
    assert!(!rig.moves(), "the key's own frame draws the deck still");
    rig.app.settle_rest(key_at + Duration::from_millis(1));
    assert_eq!(
        rig.frames_asked(),
        frames,
        "the next settle finds it attended"
    );
    let landed = key_at + Duration::from_millis(300);
    rig.app.vitals.landed_at = Some(landed);
    rig.app.vitals.sampled_at = rig.app.vitals.landed_at;
    assert!(rig.moves(), "the reading the touch asked brings the motion");
    assert_eq!(
        rig.motion_deadlines(landed),
        1,
        "the live edge's next frame"
    );

    // The control: a window that lost the focus and has it back moves on the focus's own turn.
    let mut focus = Rig::new();
    let wid = focus.wid;
    focus.app.windows.get_mut(&wid).expect("window").focused = false;
    assert_eq!(focus.motion_deadlines(focus.t0), 0, "unfocused");
    focus.app.windows.get_mut(&wid).expect("window").focused = true;
    assert_eq!(
        focus.motion_deadlines(focus.t0),
        1,
        "focused again: the live edge at once"
    );
}

/// A focused window that came to rest past its attention and is woken by its tab's output alone
/// keeps its deck still and asks no reading: the output wakes it, so it rests no longer, but
/// attends nothing, so the readings keep the background's audience and a reading that lands
/// after that wake draws the deck still. A key aterm posts for a control client wakes it the
/// same way and asks none either. The next key a person types attends it and asks a reading at
/// once, and the reading that lands after that key brings the motion back.
///
/// Negative controls, each run once by hand and restored by copy: `Rest::deck_moves` without
/// its `self.attended` term fails "a reading after the output's wake draws the deck still";
/// `admit_content_redraws` asking a reading of every focused window output wakes
/// (`ws.rest.stir(now) && ws.focused` in place of `ws.rest.stir(now) &&
/// crate::vitals_host::attends_readings(ws, false)`) fails "the output's wake asks no reading";
/// `stir_rest` asking one of every woken window that is focused (`ws.focused` in place of
/// `crate::vitals_host::attends_readings(ws, false)`) fails "the posted key's wake asks no
/// reading".
#[test]
fn output_that_wakes_a_lapsed_window_leaves_its_deck_still() {
    let mut rig = Rig::new();
    let (wid, t0) = (rig.wid, rig.t0);
    rig.app.config.idle_rest_secs = Some(30.0);
    let lapse = t0 + ATTENTION;
    rig.app.settle_rest(lapse);
    let rest = rig.app.windows[&wid].rest;
    assert!(rest.resting() && !rest.attended(), "resting and unattended");
    assert!(!rig.moves());
    rig.app.service_vitals(lapse);
    assert_eq!(
        rig.app.vitals.next_due,
        Some(lapse + BACKGROUND_PERIOD),
        "the lapse's turn arms the background cadence"
    );

    // Window 0's tab shows session 0.
    let output = lapse + Duration::from_secs(2);
    assert!(
        rig.app.admit_output_redraws(0, output),
        "the output is on screen"
    );
    let rest = rig.app.windows[&wid].rest;
    assert!(
        !rest.resting() && !rest.attended(),
        "the output woke it and attended nothing"
    );
    assert_eq!(
        rig.app.vitals.next_due,
        Some(lapse + BACKGROUND_PERIOD),
        "the output's wake asks no reading"
    );
    assert_eq!(rig.app.vitals_audience(), Audience::Background);
    let landed = output + FOCUSED_PERIOD / 2;
    rig.app.vitals.land_for_test(1, landed);
    rig.app.settle_rest(landed);
    rig.app.service_vitals(landed);
    assert_eq!(
        rig.app.vitals.landed_at,
        Some(landed),
        "the turn adopted it"
    );
    assert!(
        !rig.moves(),
        "a reading after the output's wake draws the deck still"
    );

    // Quiet again for the whole rest, then woken by a key aterm posted.
    let rest_again = output + Duration::from_secs(30);
    rig.app.settle_rest(rest_again);
    assert!(rig.app.windows[&wid].rest.resting(), "resting again");
    rig.app.service_vitals(rest_again);
    assert_eq!(rig.app.timed_reading_asked(), Some(Some(rest_again)));
    let posted = rest_again + Duration::from_secs(1);
    rig.app.input_origin = InputOrigin::Posted;
    rig.app
        .note_rest_event(wid, &crate::window_rest::tests::key(), posted);
    rig.app.input_origin = InputOrigin::Person;
    let rest = rig.app.windows[&wid].rest;
    assert!(
        !rest.resting() && !rest.attended(),
        "the posted key woke it and attended nothing"
    );
    assert_eq!(
        rig.app.vitals.next_due,
        Some(rest_again + BACKGROUND_PERIOD),
        "the posted key's wake asks no reading"
    );

    let key_at = posted + FOCUSED_PERIOD;
    rig.app
        .note_rest_event(wid, &crate::window_rest::tests::key(), key_at);
    assert!(rig.app.windows[&wid].rest.attended(), "the key attends it");
    assert_eq!(
        rig.app.vitals.next_due,
        Some(key_at),
        "the person's key asks a reading at once"
    );
    assert!(!rig.moves(), "the key's own frame draws the deck still");
    let after = key_at + FOCUSED_PERIOD / 2;
    rig.app.vitals.land_for_test(2, after);
    rig.app.settle_rest(after);
    rig.app.service_vitals(after);
    assert_eq!(rig.app.vitals.landed_at, Some(after));
    assert!(
        rig.moves(),
        "the reading after the key brings the motion back"
    );
}

/// The touch that ends a lapse asks a reading at once. An unfocused window that output keeps
/// awake reads on the background cadence, so its next reading is armed at least half that
/// period off; the focus coming to it ends its lapse and brings the next reading to the focused
/// cadence's next half-slot, less than a period and a half after the last ask.
#[test]
fn the_focus_that_ends_a_lapse_asks_a_reading_at_once() {
    let mut rig = Rig::new();
    let wid = rig.wid;
    let asked = rig.t0 + ATTENTION + Duration::from_secs(5);
    let mut ws = rig.app.windows.get_mut(&wid).expect("the window");
    ws.focused = false;
    let _ = ws.rest.stir(asked);
    drop(ws);
    rig.app.settle_rest(asked);
    let rest = rig.app.windows[&wid].rest;
    assert!(!rest.attended() && !rest.resting(), "lapsed and awake");
    rig.app.service_vitals(asked);
    assert_eq!(rig.app.vitals_audience(), Audience::Background);
    assert_eq!(
        rig.app.timed_reading_asked(),
        Some(Some(asked)),
        "the turn asked"
    );
    let armed = rig.app.vitals.next_due.expect("the next reading is armed");
    assert!(
        armed >= asked + BACKGROUND_PERIOD / 2,
        "armed on the background cadence: {:?} after the ask",
        armed - asked
    );

    let focus = asked + Duration::from_millis(100);
    rig.app
        .note_rest_event(wid, &WindowEvent::Focused(true), focus);
    assert!(
        rig.app.windows[&wid].rest.attended(),
        "the focus is a touch"
    );
    let due = rig.app.vitals.next_due.expect("the next reading is armed");
    assert!(
        due < asked + FOCUSED_PERIOD * 3 / 2,
        "the touch asks a reading on the focused cadence: {:?} after the ask",
        due - asked
    );
}

/// A scroll over an unfocused window whose attention lapsed attends it and holds nothing, and,
/// with no aterm window focused, asks no reading: its deck draws still while it is unfocused.
/// The focus that follows moves the deck on the focus's own turn, from the reading that landed
/// before the scroll, as it moves an attended window's, and the focus asks the next reading on
/// the focused cadence (`App::touch_rest`): a focused period after the last ask, it is due at
/// once. The reading lands a frame before the focus, so the deck's beat still owes the live
/// edge a frame.
///
/// Where the window rests too (`idle_rest_secs`), the same scroll wakes it, and the wake holds
/// its deck, focused or not: the focus that follows leaves the deck still on its own turn, and
/// asks the same reading, and the first reading that lands after the scroll moves it.
///
/// Negative controls, each run once by hand and restored by copy: `Rest::touch` holding the deck
/// on every touch that ends a lapse, focused or not (`if lapsed` in place of `if lapsed &&
/// focused` before it sets `woke_at`), fails "the focus moves the deck on its own turn";
/// `Rest::stir` setting no `woke_at` fails "resting: the wake's hold stands through the focus";
/// `App::touch_rest` asking only where the touch ended a lapse (`if lapsed && …` in place of `if
/// (lapsed || focusing) && …`) fails "resting: false: the focus asks a reading on the focused
/// cadence".
#[test]
fn a_scroll_ends_an_unfocused_windows_lapse_and_its_focus_moves_the_deck() {
    for resting in [false, true] {
        let mut rig = Rig::new();
        let (wid, t0) = (rig.wid, rig.t0);
        if resting {
            rig.app.config.idle_rest_secs = Some(30.0);
        }
        rig.app.windows.get_mut(&wid).expect("the window").focused = false;
        let lapse = t0 + ATTENTION;
        rig.app.settle_rest(lapse);
        let rest = rig.app.windows[&wid].rest;
        assert!(!rest.attended());
        assert_eq!(rest.resting(), resting, "resting: {resting}");
        rig.app.service_vitals(lapse);
        assert_eq!(rig.app.vitals_audience(), Audience::Background);
        assert_eq!(rig.app.timed_reading_asked(), Some(Some(lapse)));
        let landed = lapse + FOCUSED_PERIOD - FRAME;
        rig.app.vitals.land_for_test(1, landed);
        rig.app.settle_rest(landed);
        rig.app.service_vitals(landed);
        assert_eq!(
            rig.app.vitals.landed_at,
            Some(landed),
            "the turn adopted it"
        );

        let scroll = landed + FRAME / 2;
        rig.app
            .note_rest_event(wid, &crate::window_rest::tests::scroll(), scroll);
        let rest = rig.app.windows[&wid].rest;
        assert!(
            rest.attended() && !rest.resting(),
            "the scroll attends it and wakes it"
        );
        assert_eq!(
            rig.app.vitals.next_due,
            Some(lapse + BACKGROUND_PERIOD),
            "an unfocused window's touch asks no reading"
        );
        assert!(!rig.moves(), "an unfocused window's deck draws still");

        let focus = landed + FRAME;
        rig.app
            .note_rest_event(wid, &WindowEvent::Focused(true), focus);
        assert_eq!(
            rig.app.vitals.next_due,
            Some(focus),
            "resting: {resting}: the focus asks a reading on the focused cadence"
        );
        // The event's handler stores the focus once the rest has noted it.
        rig.app.windows.get_mut(&wid).expect("the window").focused = true;
        assert_eq!(rig.app.vitals_audience(), Audience::Shown);
        if resting {
            assert!(
                !rig.moves(),
                "resting: the wake's hold stands through the focus"
            );
            assert_eq!(rig.motion_deadlines(focus), 0, "resting: no deck motion");
            let after = scroll + FRAME;
            rig.app.vitals.land_for_test(2, after);
            rig.app.settle_rest(after);
            rig.app.service_vitals(after);
            assert_eq!(rig.app.vitals.landed_at, Some(after));
            assert!(
                rig.moves(),
                "resting: the first reading after the scroll moves the deck"
            );
        } else {
            assert!(rig.moves(), "the focus moves the deck on its own turn");
            assert_eq!(rig.motion_deadlines(focus), 1, "the live edge's next frame");
        }
    }
}

/// The focus coming to an awake window that is still attended asks a reading at once, though
/// it ends no lapse: with no aterm window focused, the readings are on the background cadence,
/// the focus that brings aterm to the front gives them a person's audience, and the focused
/// cadence follows the reading it asks. A window still attended that is uncovered beside a
/// focused window whose attention lapsed gives them that audience too, and its uncover asks a
/// reading at once.
///
/// Negative controls, each run once by hand and restored by copy: `App::touch_rest` asking
/// only where the touch ended a lapse (`if lapsed && …` in place of `if (lapsed || focusing) &&
/// …`) fails "the focus asks a reading at once"; `App::on_occlusion_changed` asking nothing
/// (`let attends = false;`) fails "the uncover asks a reading at once".
#[test]
fn the_focus_or_the_uncover_of_an_attended_window_asks_a_reading_at_once() {
    let mut rig = Rig::new();
    let (wid, t0) = (rig.wid, rig.t0);
    rig.app.windows.get_mut(&wid).expect("the window").focused = false;
    let quiet = t0 + ATTENTION / 2;
    rig.app.settle_rest(quiet);
    let rest = rig.app.windows[&wid].rest;
    assert!(
        rest.attended() && !rest.resting(),
        "CONTROL: awake and attended, so the focus ends no lapse"
    );
    rig.app.service_vitals(quiet);
    assert_eq!(
        rig.app.vitals_audience(),
        Audience::Background,
        "CONTROL: no window is focused"
    );
    assert_eq!(rig.app.timed_reading_asked(), Some(Some(quiet)));
    assert_eq!(
        rig.app.vitals.next_due,
        Some(quiet + BACKGROUND_PERIOD),
        "CONTROL: the background cadence"
    );
    let focus = quiet + FOCUSED_PERIOD * 2;
    rig.app
        .note_rest_event(wid, &WindowEvent::Focused(true), focus);
    assert_eq!(
        rig.app.vitals.next_due,
        Some(focus),
        "the focus asks a reading at once"
    );
    // The event's handler stores the focus once the rest has noted it.
    rig.app.windows.get_mut(&wid).expect("the window").focused = true;
    assert_eq!(rig.app.vitals_audience(), Audience::Shown);
    rig.app.service_vitals(focus);
    assert_eq!(rig.app.timed_reading_asked(), Some(Some(focus)));
    assert_eq!(
        rig.app.vitals.next_due,
        Some(focus + FOCUSED_PERIOD),
        "the focused cadence follows"
    );

    let mut rig = Rig::new();
    let (wid, t0) = (rig.wid, rig.t0);
    let covered = crate::WindowId(1);
    rig.app
        .install_window_state(covered, crate::stub_session(93), 24, 80);
    let mut ws = rig
        .app
        .windows
        .get_mut(&covered)
        .expect("the covered window");
    ws.focused = false;
    ws.occluded = true;
    drop(ws);
    rig.app.note_rest_event(
        covered,
        &crate::window_rest::tests::scroll(),
        t0 + ATTENTION / 2,
    );
    let lapse = t0 + ATTENTION;
    rig.app.settle_rest(lapse);
    let attended = |rig: &Rig, wid: crate::WindowId| rig.app.windows[&wid].rest.attended();
    assert!(
        !attended(&rig, wid) && attended(&rig, covered),
        "CONTROL: the focused window lapsed, the covered one is attended"
    );
    rig.app.service_vitals(lapse);
    assert_eq!(
        rig.app.vitals_audience(),
        Audience::Background,
        "CONTROL: a covered unfocused window gives no audience"
    );
    assert_eq!(rig.app.vitals.next_due, Some(lapse + BACKGROUND_PERIOD));
    let uncover = lapse + FOCUSED_PERIOD * 2;
    rig.app.on_occlusion_changed(covered, false, uncover);
    assert_eq!(
        rig.app.vitals.next_due,
        Some(uncover),
        "the uncover asks a reading at once"
    );
    assert_eq!(rig.app.vitals_audience(), Audience::Shown);
}

/// A person who scrolls a window beside the focused one, on screen, keeps the focused cadence
/// once the focused window's attention lapsed: the scroll attends that window, which gives the
/// readings a person's audience (`vitals_host::attends_readings`), so the stream reads as shown
/// and the scroll asks a reading at once, while the focused window's deck stays still, on the
/// reading the scroll asked too: the scroll attends only the window it touched, which leaves
/// the focused window unattended. An occluded window gives no audience, and its scroll asks
/// nothing.
///
/// Negative controls, each run once by hand and restored by copy: `vitals_audience` counting
/// the focused window alone (`self.windows.values().any(|ws| ws.focused && (page ||
/// attends_readings(ws, false)))` in place of `front && (page ||
/// self.windows.values().any(|ws| attends_readings(ws, false)))`) fails "a touch beside the
/// lapsed focused window keeps the focused cadence", left `Background`, right `Shown`;
/// `App::touch_rest` also touching each focused window (`rest.touch(now, true)`) where it ends
/// another window's lapse fails "assertion failed: attended(&rig, beside) && !attended(&rig,
/// wid)"; and `App::live_facts` also moving a focused window's deck that does not rest on any
/// reading adopted at the focused cadence (`|| (!ws.rest.resting() &&
/// self.vitals.landed_period == Some(FOCUSED_PERIOD))` beside `deck_moves`) fails "the focused
/// window's deck stays still on the reading the scroll asked".
#[test]
fn a_touch_beside_a_lapsed_focused_window_keeps_the_focused_cadence() {
    let mut rig = Rig::new();
    let (wid, t0) = (rig.wid, rig.t0);
    let beside = crate::WindowId(1);
    rig.app
        .install_window_state(beside, crate::stub_session(91), 24, 80);
    let mut ws = rig.app.windows.get_mut(&beside).expect("the window beside");
    ws.focused = false;
    ws.occluded = false;
    drop(ws);
    rig.app
        .note_rest_event(beside, &crate::window_rest::tests::scroll(), t0);
    let lapse = t0 + ATTENTION;
    rig.app.settle_rest(lapse);
    let attended = |rig: &Rig, wid: crate::WindowId| rig.app.windows[&wid].rest.attended();
    assert!(!attended(&rig, wid) && !attended(&rig, beside));
    assert_eq!(
        rig.app.vitals_audience(),
        Audience::Background,
        "CONTROL: neither window attended"
    );
    rig.app.service_vitals(lapse);
    assert_eq!(rig.app.vitals.next_due, Some(lapse + BACKGROUND_PERIOD));

    let scroll = lapse + Duration::from_secs(2);
    rig.app
        .note_rest_event(beside, &crate::window_rest::tests::scroll(), scroll);
    assert!(attended(&rig, beside) && !attended(&rig, wid));
    assert_eq!(
        rig.app.vitals_audience(),
        Audience::Shown,
        "a touch beside the lapsed focused window keeps the focused cadence"
    );
    assert_eq!(
        rig.app.vitals.next_due,
        Some(scroll),
        "the scroll asks a reading at once"
    );
    assert!(!rig.moves(), "the focused window's deck stays still");
    rig.app.service_vitals(scroll);
    assert_eq!(rig.app.timed_reading_asked(), Some(Some(scroll)));
    assert_eq!(
        rig.app.vitals.next_due,
        Some(scroll + FOCUSED_PERIOD),
        "the focused cadence follows"
    );
    // The probe thread hands over the reading the scroll asked, and its turn adopts it.
    let landed = scroll + FOCUSED_PERIOD / 4;
    rig.app.vitals.land_for_test(1, landed);
    rig.app.settle_rest(landed);
    rig.app.service_vitals(landed);
    assert_eq!(
        rig.app.vitals.landed_at,
        Some(landed),
        "the turn adopted it"
    );
    assert!(
        !rig.moves(),
        "the focused window's deck stays still on the reading the scroll asked"
    );

    let covered = scroll + ATTENTION;
    rig.app.settle_rest(covered);
    assert!(!attended(&rig, beside), "lapsed again");
    rig.app.service_vitals(covered);
    assert_eq!(rig.app.vitals.next_due, Some(covered + BACKGROUND_PERIOD));
    rig.app
        .windows
        .get_mut(&beside)
        .expect("the window beside")
        .occluded = true;
    let hidden_scroll = covered + Duration::from_secs(2);
    rig.app
        .note_rest_event(beside, &crate::window_rest::tests::scroll(), hidden_scroll);
    assert!(attended(&rig, beside), "the scroll attends it");
    assert_eq!(
        rig.app.vitals_audience(),
        Audience::Background,
        "an occluded window gives no audience"
    );
    assert_eq!(
        rig.app.vitals.next_due,
        Some(covered + BACKGROUND_PERIOD),
        "and its scroll asks nothing"
    );
}

/// A resize a control client asked for attends nothing: the `Resized` that reports the size
/// aterm asked the window for, in either form of the `resize` verb, is aterm's own. A resize
/// to any other size is a person's and attends the window.
#[test]
fn a_control_clients_resize_attends_nothing_and_a_persons_does() {
    let mut rig = Rig::new();
    let (wid, t0) = (rig.wid, rig.t0);
    rig.app.frontmost_window = Some(wid);
    let lapse = t0 + ATTENTION;
    rig.app.settle_rest(lapse);
    assert!(!rig.app.windows[&wid].rest.attended());

    let outcome = rig.app.input(
        wid,
        InputEvent::ResizeWindowPx {
            width: 1_000,
            height: 700,
        },
        Source::Controller,
    );
    let InputOutcome::Resized(WindowResize::Px { given, .. }) = outcome else {
        panic!("the pixel form resizes the window: {outcome:?}");
    };
    let px = PhysicalSize::new(given.0, given.1);
    rig.app
        .note_rest_event(wid, &WindowEvent::Resized(px), lapse);
    assert!(
        !rig.app.windows[&wid].rest.attended(),
        "the pixel form's own resize attended nothing"
    );

    let (rows, cols) = (30, 100);
    let outcome = rig.app.input(
        wid,
        InputEvent::Resize {
            rows,
            cols,
            echo_to_window: true,
        },
        Source::Controller,
    );
    let InputOutcome::Resized(WindowResize::Grid { given, .. }) = outcome else {
        panic!("the grid form resizes the window: {outcome:?}");
    };
    let grid = rig.app.window_frame_px_for(wid, given.0, given.1);
    assert_ne!(grid, px, "the grid form asked for another size");
    rig.app
        .note_rest_event(wid, &WindowEvent::Resized(grid), lapse);
    assert!(
        !rig.app.windows[&wid].rest.attended(),
        "the grid form's own resize attended nothing"
    );

    let dragged = PhysicalSize::new(grid.width + 1, grid.height);
    rig.app
        .note_rest_event(wid, &WindowEvent::Resized(dragged), lapse);
    assert!(
        rig.app.windows[&wid].rest.attended(),
        "a resize to a size nobody asked aterm for is a person's"
    );
}

/// What attends: a person's event, and one another process posted; not a key aterm posted for
/// a control client, not a frame, not the focus leaving.
#[test]
fn a_posted_key_a_frame_and_the_focus_leaving_attend_nothing() {
    let mut rig = Rig::new();
    let (wid, t0) = (rig.wid, rig.t0);
    let late = t0 + ATTENTION - Duration::from_secs(1);
    rig.app.input_origin = InputOrigin::Posted;
    rig.app
        .note_rest_event(wid, &crate::window_rest::tests::key(), late);
    rig.app.input_origin = InputOrigin::Person;
    rig.app
        .note_rest_event(wid, &WindowEvent::RedrawRequested, late);
    rig.app
        .note_rest_event(wid, &WindowEvent::Focused(false), late);
    rig.app.settle_rest(t0 + ATTENTION);
    assert!(
        !rig.app.windows[&wid].rest.attended(),
        "none of the three was a person's touch"
    );
    rig.app.input_origin = InputOrigin::Relayed;
    rig.app
        .note_rest_event(wid, &crate::window_rest::tests::key(), t0 + ATTENTION);
    rig.app.input_origin = InputOrigin::Person;
    assert!(
        rig.app.windows[&wid].rest.attended(),
        "a key another process posted may be a person's"
    );
    rig.app.settle_rest(t0 + ATTENTION * 2);
    assert!(!rig.app.windows[&wid].rest.attended());
    rig.app
        .note_rest_event(wid, &WindowEvent::Focused(true), t0 + ATTENTION * 2);
    assert!(rig.app.windows[&wid].rest.attended(), "the focus arriving");
}

/// A headless instance and a window under `motion = "full"` are attended however long nobody
/// touches them.
#[test]
fn a_headless_instance_and_full_motion_are_always_attended() {
    let mut rig = Rig::new();
    let long = rig.t0 + ATTENTION * 10;
    rig.app.headless = true;
    rig.app.settle_rest(long);
    assert!(rig.app.windows[&rig.wid].rest.attended(), "headless");
    assert!(rig.moves());
    rig.app.headless = false;
    rig.app.config.motion = Some("full".into());
    rig.app.settle_rest(long);
    assert!(rig.app.windows[&rig.wid].rest.attended(), "motion = full");
    assert!(rig.moves());
    let frames = rig.frames_asked();
    rig.app.config.motion = None;
    rig.app.settle_rest(long);
    assert!(!rig.app.windows[&rig.wid].rest.attended(), "the default");
    assert_eq!(rig.frames_asked(), frames + 1);
}

/// A touch gives a lapsed window's settled pet its offer back in the touch's own turn: the plan
/// right after a person's key returns the step the lapse withheld, and the touch asks no frame.
/// A key aterm posted for a control client attends nothing, and the pet stays still.
///
/// NEGATIVE CONTROL: the admission read a snapshot `settle_rest` took (a `WindowState` bit set
/// to `ws.person_here()` in `settle_rest`, read by `pet_offer_admitted` in place of
/// `self.person_here()`): "a person's key gives the pet its step in the key's own turn" failed
/// (left None, right Some(Instant { .. })).
#[test]
fn a_touch_restores_the_pet_offer_in_its_own_turn() {
    let mut app = crate::App::headless_for_test();
    let wid = crate::WindowId(0);
    app.headless = false;
    let start = Instant::now() + ATTENTION;
    let mut ws = app.windows.get_mut(&wid).expect("the headless window");
    let now = crate::effect_lane_attended_tests::settled_pet(&mut ws, start);
    let offer = ws
        .fx
        .companion()
        .next_change_deadline(now)
        .expect("fixture: the settled resident owes a step");
    drop(ws);
    let plan = |app: &mut crate::App| {
        app.windows
            .get_mut(&wid)
            .expect("the window")
            .plan_terminal_effect_lane_with_recording(now, false, true, true, false, 10.0, true)
    };
    app.settle_rest(now);
    assert!(
        !app.windows[&wid].rest.attended(),
        "fixture: the attention lapsed"
    );
    assert_eq!(
        plan(&mut app),
        None,
        "a lapsed window's settled pet is still"
    );

    app.input_origin = InputOrigin::Posted;
    app.note_rest_event(wid, &crate::window_rest::tests::key(), now);
    app.input_origin = InputOrigin::Person;
    assert_eq!(
        plan(&mut app),
        None,
        "a key aterm posted gives the pet nothing"
    );

    let asks = crate::frame_asks_for_test(&app);
    app.note_rest_event(wid, &crate::window_rest::tests::key(), now);
    assert_eq!(
        crate::frame_asks_for_test(&app),
        asks,
        "the touch asks no frame"
    );
    assert_eq!(
        plan(&mut app),
        Some(offer),
        "a person's key gives the pet its step in the key's own turn"
    );
}
