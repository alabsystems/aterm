// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! The readings' clock follows a person's attention: a focused window nobody has touched for
//! [`ATTENTION`] is read as a resting one is, the touch that ends the lapse asks a reading at
//! once and brings the focused cadence back, output or a key aterm posted that wakes such a
//! window asks none, and a page on screen, an open stats lens, a headless instance and
//! `motion = "full"` keep the cadence they had. Each law counts the readings the loop's own
//! functions ask ([`crate::App::settle_rest`], [`crate::App::service_vitals`],
//! [`crate::App::note_rest_event`], `admit_output_redraws`) on a clock the test steps from one
//! deadline the loop folds ([`crate::App::vitals_deadline`]) to the next, with no wall clock
//! ([`super::Wall::Unread`]: each reading is due a period after the last ask).

use std::sync::mpsc::Receiver;
use std::time::{Duration, Instant};

use aterm_window::event::InputOrigin;

use super::{BACKGROUND_PERIOD, FOCUSED_PERIOD, Msg};
use crate::native_settings::SettingsRoute;
use crate::window_rest::ATTENTION;

/// A focused window on screen drawing the deck, touched at `t0`, whose readings the test's
/// stand-in reader takes.
struct Rig {
    app: crate::App,
    wid: crate::WindowId,
    t0: Instant,
    /// The instant of the loop's last turn.
    at: Instant,
    rx: Receiver<Msg>,
    _attached: crate::stats_deck::tests::Attached,
}

impl Rig {
    fn new() -> Self {
        let attached = crate::stats_deck::tests::attach_stats();
        let t0 = Instant::now();
        let (mut app, wid) = crate::stats_live::tests::live_app(t0);
        app.headless = false;
        let rx = app.vitals.attach_test_reader();
        app.note_rest_event(wid, &crate::window_rest::tests::key(), t0);
        Self {
            app,
            wid,
            t0,
            at: t0,
            rx,
            _attached: attached,
        }
    }

    /// One turn of the loop at `at`, as `new_events` runs it: every window's rest and attention
    /// settled, then the readings served. The readings it asked, as scripted ms from `t0`.
    fn turn(&mut self, at: Instant, asked: &mut Vec<u64>) {
        self.at = at;
        self.app.settle_rest(at);
        self.app.service_vitals(at);
        let ms = u64::try_from(at.duration_since(self.t0).as_millis()).expect("ms");
        asked.extend(
            self.rx
                .try_iter()
                .filter(|m| matches!(m, Msg::Read(_)))
                .map(|_| ms),
        );
    }

    /// Runs the loop from its last turn to `until` (scripted seconds, exclusive): a turn at each
    /// deadline the readings arm, and at each of `touches` (scripted seconds) a turn whose event
    /// is a person's key in the window, handled after the turn's top as a window event is. The
    /// first call's first turn is at `t0`. The readings asked, as scripted ms from `t0`.
    fn run(&mut self, until: Duration, touches: &[Duration]) -> Vec<u64> {
        let t0 = self.t0;
        let end = t0 + until;
        let mut touches = touches.iter().map(|d| t0 + *d).peekable();
        let mut asked = Vec::new();
        if self.at == self.t0 && self.app.vitals.asked_at.is_none() {
            self.turn(self.t0, &mut asked);
        }
        for _ in 0..10_000 {
            let due = self
                .app
                .vitals_deadline()
                .expect("a window that shows the bars keeps a reading armed")
                .max(self.at);
            if let Some(touch) = touches.next_if(|t| *t <= due && *t < end) {
                self.turn(touch, &mut asked);
                self.app
                    .note_rest_event(self.wid, &crate::window_rest::tests::key(), touch);
                continue;
            }
            if due >= end {
                return asked;
            }
            self.turn(due, &mut asked);
        }
        panic!("the loop never reached {until:?}: {asked:?}");
    }
}

/// Whole scripted seconds, as ms.
fn secs(list: impl IntoIterator<Item = u64>) -> Vec<u64> {
    list.into_iter().map(|s| s * 1_000).collect()
}

/// The schedule of an untouched focused window over 120 scripted seconds, touched at second 0:
/// a reading a second while it is attended, seconds 0 to 40 (41 readings: the one at 40 is the
/// one the focused cadence armed at 39, the turn the lapse is settled in), then one every
/// `BACKGROUND_PERIOD`, 45 to 115 (15): 56 readings. A touch at 102 (between two resting
/// readings, 100 and 105) asks one at once, at 102, and the focused cadence is back from
/// there, 103 to 119 (17): 71 readings. The deck holds still until a reading lands after the
/// touch, and moves from that one.
///
/// Negative controls, each applied once by hand: the audience ignoring the attention (a reading
/// every second throughout, 120); the touch asking no reading (the next is the resting
/// cadence's at 105, three seconds after the touch).
#[test]
fn an_unattended_focused_window_is_read_on_the_resting_clock_and_a_touch_asks_at_once() {
    assert_eq!(ATTENTION, Duration::from_secs(40));
    assert_eq!(
        (FOCUSED_PERIOD, BACKGROUND_PERIOD),
        (Duration::from_secs(1), Duration::from_secs(5))
    );
    let untouched = Rig::new().run(Duration::from_secs(120), &[]);
    let resting = secs((0..=40).chain((45..=115).step_by(5)));
    assert_eq!(resting.len(), 56);
    assert_eq!(untouched, resting, "untouched for 120 s");

    let mut rig = Rig::new();
    let touch = Duration::from_secs(102);
    let mut asked = rig.run(touch + Duration::from_millis(1), &[touch]);
    assert_eq!(
        asked,
        secs((0..=40).chain((45..=100).step_by(5)).chain([102])),
        "the touch at 102 asks one reading at once"
    );
    let wid = rig.wid;
    let moves = |rig: &Rig| rig.app.live_facts(wid).expect("the deck draws").motion;
    assert!(rig.app.windows[&wid].rest.attended(), "the key attends it");
    assert!(!moves(&rig), "no reading has landed since the touch");
    // The reading the touch asked lands, and its `Wake::Vitals` turn adopts it.
    let landed = rig.t0 + touch + Duration::from_millis(300);
    rig.app.vitals.land_for_test(1, rig.t0 + touch);
    rig.turn(landed, &mut asked);
    assert_eq!(rig.app.vitals.landed_at, Some(landed));
    assert!(
        moves(&rig),
        "the deck moves from the reading the touch asked"
    );
    asked.extend(rig.run(Duration::from_secs(120), &[]));
    let touched = secs(
        (0..=40)
            .chain((45..=100).step_by(5))
            .chain([102])
            .chain(103..=119),
    );
    assert_eq!(touched.len(), 71);
    assert_eq!(asked, touched, "the focused cadence from the touch on");
}

/// With `idle_rest_secs` at 30 the window rests at 30 (the reading there is the one the focused
/// cadence armed at 29) and its attention lapses at 40; at 102 `wake` wakes it, nobody touching
/// it. The readings asked over 120 scripted seconds.
fn woken_unattended_at_102(
    wake: impl FnOnce(&mut crate::App, crate::WindowId, Instant),
) -> Vec<u64> {
    let mut rig = Rig::new();
    rig.app.config.idle_rest_secs = Some(30.0);
    let woke = Duration::from_secs(102);
    let mut asked = rig.run(woke, &[]);
    let at = rig.t0 + woke;
    rig.turn(at, &mut asked);
    let wid = rig.wid;
    assert!(rig.app.windows[&wid].rest.resting(), "rests from 30");
    wake(&mut rig.app, wid, at);
    assert!(!rig.app.windows[&wid].rest.resting(), "it woke");
    assert!(!rig.app.windows[&wid].rest.attended(), "nobody touched it");
    asked.extend(rig.run(Duration::from_secs(120), &[]));
    asked
}

/// Output that wakes a resting window nobody attends asks no reading: its cadence stays the
/// resting one and its deck holds still. Its tab's output wakes it at 102 through the loop's
/// own output admission (`admit_output_redraws`, which `on_output_wake` calls): readings at 0
/// to 30 and then every `BACKGROUND_PERIOD`, 35 to 115, 48 in 120 s, none at 102.
///
/// Negative control, applied once by hand: the output's wake asks a reading whatever the
/// attention (one at 102, and the resting cadence from there: 107, 112, 117).
#[test]
fn output_that_wakes_an_unattended_window_asks_no_reading() {
    let asked = woken_unattended_at_102(|app, _, at| {
        // The rig's window 0 shows session 0 in its active tab.
        assert!(
            app.admit_output_redraws(0, at),
            "the window shows the output"
        );
    });
    let resting = secs((0..=30).chain((35..=115).step_by(5)));
    assert_eq!(resting.len(), 48);
    assert_eq!(asked, resting, "output at 102 asks none");
}

/// A key aterm posted for a control client (an `hwkey` burst) wakes a resting window and
/// touches nothing ([`crate::App::note_rest_event`]); where nobody attends the window, that wake
/// asks no reading either ([`crate::App::stir_rest`]): the same 48 readings, none at 102.
///
/// Negative control, applied once by hand: `stir_rest` asks a reading whatever the attention
/// (one at 102, then 107, 112, 117).
#[test]
fn a_posted_key_that_wakes_an_unattended_window_asks_no_reading() {
    let asked = woken_unattended_at_102(|app, wid, at| {
        app.input_origin = InputOrigin::Posted;
        app.note_rest_event(wid, &crate::window_rest::tests::key(), at);
        app.input_origin = InputOrigin::Person;
    });
    assert_eq!(
        asked,
        secs((0..=30).chain((35..=115).step_by(5))),
        "a posted key at 102 asks none"
    );
}

/// What keeps its cadence however long nobody touches the window: a Tab Control page on
/// screen (in another window) keeps the focused one, a reading a second (120 in 120 s), as it
/// does beside a resting window; an open stats lens its own, a reading every
/// `LENS_PERIOD` (240); a headless instance and `motion = "full"`, both always attended, the
/// focused one (120). The filmed window is attended the same way
/// (`lib_headless_video_tests::a_filmed_window_never_rests_while_another_does`). The window
/// that carries the page is on screen (and focused, as `install_window_state` makes it), and a
/// window on screen that a person attends keeps the focused cadence of its own
/// (`vitals_host::attends_readings`), so it is touched at 0 with the rig's window and both
/// lapse at 40, whatever the time building it took: past that only the page keeps the cadence.
///
/// Negative control, applied once by hand: the attention's lapse slows the stream whatever a
/// page reads (`vitals_audience` without its `page ||` term): "a page on screen" fails with
/// the untouched window's 56 (a reading a second to 40, then one every `BACKGROUND_PERIOD` from
/// 45) where 120.
#[test]
fn a_page_a_lens_a_headless_instance_and_full_motion_keep_their_cadence_unattended() {
    let every_second = secs(0..120);
    let span = Duration::from_secs(120);

    let mut page = Rig::new();
    let beside = crate::WindowId(1);
    page.app
        .install_window_state(beside, crate::stub_session(91), 24, 80);
    assert!(
        page.app
            .open_settings_tab_in_window(beside, SettingsRoute::Tabs)
    );
    page.app.frontmost_window = Some(page.wid);
    page.app
        .note_rest_event(beside, &crate::window_rest::tests::key(), page.t0);
    assert!(
        !page.app.windows[&beside].occluded,
        "CONTROL: the page's window is on screen"
    );
    assert!(page.app.tabs_page_on_screen());
    assert_eq!(page.run(span, &[]), every_second, "a page on screen");
    assert!(!page.app.windows[&page.wid].rest.attended());
    assert!(
        !page.app.windows[&beside].rest.attended(),
        "the page's window lapsed too"
    );

    let mut lens = Rig::new();
    lens.app
        .windows
        .get_mut(&lens.wid)
        .expect("the window")
        .stats_lens
        .open = Some(crate::stats_lens::Lens {
        focus: crate::stats_paint::Instrument::Cpu,
        bar: crate::stats_paint::Bar::Global,
        how: crate::stats_lens::Opened::Hover,
    });
    let half_seconds: Vec<u64> = (0..240).map(|k| k * 500).collect();
    assert_eq!(crate::stats_lens::LENS_PERIOD, Duration::from_millis(500));
    assert_eq!(lens.run(span, &[]), half_seconds, "an open stats lens");
    assert!(!lens.app.windows[&lens.wid].rest.attended());

    let mut headless = Rig::new();
    headless.app.headless = true;
    assert_eq!(headless.run(span, &[]), every_second, "a headless instance");
    assert!(headless.app.windows[&headless.wid].rest.attended());

    let mut full = Rig::new();
    full.app.config.motion = Some("full".into());
    assert_eq!(full.run(span, &[]), every_second, "motion = \"full\"");
    assert!(full.app.windows[&full.wid].rest.attended());
}
