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

/// Past its attention the deck draws still: one frame is asked at the lapse, the live edge and
/// the deck clock arm nothing, only the grey's moment stays scheduled, and the readings are on
/// the resting cadence (`vitals_host_attended_tests` counts them). Each reading that lands
/// after it asks one frame, which draws the deck still on that reading, and no turn asks
/// another. Output in between changes none of it.
#[test]
fn an_unattended_windows_deck_is_still_and_draws_each_reading_once() {
    let mut rig = Rig::new();
    let t0 = rig.t0;
    assert!(rig.moves());
    assert!(
        rig.app
            .stats_live_deadline(t0)
            .is_some_and(|at| at > t0 && at <= t0 + FRAME),
        "the live edge's next frame"
    );
    let frames = rig.frames_asked();

    let output = t0 + Duration::from_secs(30);
    let mut ws = rig.app.windows.get_mut(&rig.wid).expect("the window");
    assert!(!ws.rest.stir(output), "output stirs an awake window");
    drop(ws);
    rig.app
        .settle_rest(t0 + ATTENTION - Duration::from_millis(1));
    assert!(rig.app.windows[&rig.wid].rest.attended());
    assert!(rig.moves(), "a millisecond short of its attention");
    assert_eq!(rig.frames_asked(), frames);

    let lapse = t0 + ATTENTION;
    rig.app.settle_rest(lapse);
    assert!(
        !rig.app.windows[&rig.wid].rest.attended(),
        "output 10 s ago attended nothing"
    );
    assert_eq!(rig.frames_asked(), frames + 1, "one frame for the lapse");
    let still = rig.app.live_facts(rig.wid).expect("the deck still draws");
    assert!(!still.motion, "{still:?}");
    assert_eq!(still.live(t0, OPEN), DeckLive::STILL);
    assert_eq!(
        still.stale_after,
        BACKGROUND_PERIOD * STALE_PERIODS,
        "the readings' cadence is the resting one"
    );
    assert_eq!(rig.app.vitals_audience(), Audience::Background);
    assert_eq!(
        rig.app.stats_live_deadline(t0),
        Some(t0 + still.stale_after),
        "only the grey is scheduled"
    );
    assert_eq!(rig.app.deck_clock_deadline(t0), None);
    for k in 1..=8_u32 {
        // The probe thread hands the reading over, and its `Wake::Vitals` turn settles the
        // rest and serves the readings, as every turn does.
        let at = lapse + BACKGROUND_PERIOD * k;
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
        let still = rig.app.live_facts(rig.wid).expect("the deck still draws");
        assert!(!still.motion, "reading {k}: {still:?}");
        assert_eq!(
            rig.motion_deadlines(at),
            0,
            "reading {k}: no motion deadline"
        );
        // A turn between two readings: nothing landed, so nothing is drawn.
        let between = at + BACKGROUND_PERIOD / 4;
        rig.app.settle_rest(between);
        rig.app.service_vitals(between);
        assert_eq!(
            rig.frames_asked(),
            frames + 1 + k,
            "reading {k}: a turn with no reading asks no frame"
        );
    }
}

/// The deck clock's port, ticked by hand: a window that has a display link; it reaches none.
fn inert_link(_: Option<&aterm_window::window::Window>, _: bool) -> bool {
    true
}

/// An unattended window's deck clock wants no tick while its needle owes a sweep, as an
/// occluded or unfocused one's wants none: the lapse pauses the running link and arms no
/// deadline, the touch that ends the lapse runs nothing on its own, and the sweep of the first
/// reading that lands after the touch runs the link again. This deck's needle sweeps from a
/// calm reading to a busy one landed a second later, on a clock a whole attention past the
/// window's opening.
#[test]
fn an_unattended_windows_sweeping_needle_runs_no_display_link() {
    use crate::stats_live::tests::{busier, fit_deck, live_app};
    let _attached = crate::stats_deck::tests::attach_stats();
    let t0 = Instant::now() + ATTENTION;
    let (mut app, wid) = live_app(t0);
    app.headless = false;
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
/// first reading that lands after the key. A focused window's next reading is at most a
/// focused period off whatever the key does; the reading the touch itself asks is
/// `the_focus_that_ends_a_lapse_asks_a_reading_at_once`.
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
