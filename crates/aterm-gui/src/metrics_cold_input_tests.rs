// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use super::{
    COLD_GAP_NS, Metrics, PendingChromeInputStamp, PresentClock, PresentInput, PresentObserver,
    WindowPresentState, inert_text, note_prediction_shown, text_field,
};
use std::time::{Duration, Instant};

const MS: u64 = 1_000_000;
const T0: u64 = 10_000 * MS;

/// A present at `now` (epoch `epoch`), with `key` typed 2 ms before it when `key` is set: an
/// echo's content present, or a frame that books no input.
fn present_at(
    m: &Metrics,
    now: u64,
    epoch: u64,
    key: bool,
    window: &mut WindowPresentState,
    observer: &mut PresentObserver,
) -> u32 {
    if key {
        assert!(window.pending_input.arm(now - 2 * MS, now - 2 * MS));
    }
    let clock = PresentClock {
        now,
        epoch,
        capture_forced: false,
        tainted: false,
    };
    let latency = if key { MS } else { 0 };
    m.record_present_at(clock, latency, MS, Some(window), observer)
        .input_booked
}

fn cold(m: &Metrics) -> u64 {
    m.window.input.input_cold.copy_out().n()
}

/// The key's present after `gap` of quiet in a one-window process, from a fresh ledger: the
/// number of cold slices it booked.
fn cold_after(gap: u64) -> u64 {
    let m = Metrics::new();
    let mut window = WindowPresentState::default();
    let mut observer = PresentObserver::default();
    let _ = present_at(&m, T0, 0, false, &mut window, &mut observer);
    assert_eq!(
        present_at(&m, T0 + gap, 0, true, &mut window, &mut observer),
        1
    );
    cold(&m)
}

#[test]
fn a_key_after_two_seconds_of_quiet_books_a_cold_slice_and_one_before_does_not() {
    assert_eq!(COLD_GAP_NS, 2_000 * MS);
    assert_eq!(cold_after(2_000 * MS), 1, "a whole 2 s of quiet is cold");
    assert_eq!(cold_after(1_900 * MS), 0, "1.9 s is warm");

    let m = Metrics::new();
    let mut window = WindowPresentState::default();
    let mut observer = PresentObserver::default();
    assert_eq!(present_at(&m, T0, 0, true, &mut window, &mut observer), 1);
    assert_eq!(
        cold(&m),
        0,
        "the process's first present has no gap to be cold by"
    );
    assert_eq!(
        present_at(&m, T0 + 3_000 * MS, 0, false, &mut window, &mut observer),
        0
    );
    assert_eq!(
        cold(&m),
        0,
        "a cold present that books no input books no cold slice"
    );
    assert_eq!(m.window.input.input.copy_out().n(), 1);
}

#[test]
fn the_first_key_after_a_reset_reads_the_gap_before_the_reset() {
    let m = Metrics::new();
    let mut window = WindowPresentState::default();
    let mut observer = PresentObserver::default();
    let _ = present_at(&m, T0, 0, false, &mut window, &mut observer);
    let epoch = T0 + 2_500 * MS;
    assert_eq!(
        present_at(&m, T0 + 3_000 * MS, epoch, true, &mut window, &mut observer),
        1
    );
    assert_eq!(
        cold(&m),
        1,
        "the previous present 3 s earlier predates the reset and still counts"
    );
    let text = inert_text(&m, 0);
    for key in ["last_input_cold_ms", "max_input_cold_ms"] {
        assert_eq!(
            text_field(&text, key).and_then(|value| value.parse::<f64>().ok()),
            Some(2.0),
            "the cold key's actual slice reaches both surfaces: {key}: {text}"
        );
        assert_eq!(
            text.split_whitespace()
                .filter(|field| field.split_once('=').is_some_and(|(name, _)| name == key))
                .count(),
            2,
            "the cold slice is published by metrics and metrics percentiles: {key}: {text}"
        );
    }
    assert_eq!(text_field(&text, "n_input_cold"), Some("1"), "{text}");
    assert_eq!(
        text.split_whitespace()
            .filter(|f| f.starts_with("n_input_cold="))
            .count(),
        2,
        "beside n_input in `metrics` and in `metrics percentiles`: {text}"
    );
    assert_eq!(
        text_field(&text, "input_cold_p95_ms"),
        text_field(&text, "input_p95_ms"),
        "the one key is both input and cold input, read at the same resolution: {text}"
    );
}

#[test]
fn another_windows_present_keeps_the_process_warm() {
    let m = Metrics::new();
    let (mut a, mut b) = (WindowPresentState::default(), WindowPresentState::default());
    let mut observer = PresentObserver::default();
    let _ = present_at(&m, T0, 0, false, &mut a, &mut observer);
    let _ = present_at(&m, T0 + 2_000 * MS, 0, false, &mut b, &mut observer);
    assert_eq!(
        present_at(&m, T0 + 3_000 * MS, 0, true, &mut a, &mut observer),
        1
    );
    assert_eq!(
        cold(&m),
        0,
        "A was quiet 3 s, but B presented 1 s before: the GPU was not quiet"
    );
}

/// An App whose window 0 rests, focused or not, and the instant it came to rest.
fn resting_app(focused: bool) -> (crate::App, crate::WindowId, Instant) {
    let mut app = crate::App::headless_for_test();
    // Rest is for a window on screen: a headless instance's never rests. It is off unless set.
    app.headless = false;
    app.config.idle_rest_secs = Some(30.0);
    let wid = crate::WindowId(0);
    app.windows.get_mut(&wid).expect("the window").focused = focused;
    let at = Instant::now() + Duration::from_secs(31);
    app.settle_rest(at);
    assert!(app.windows[&wid].rest.resting());
    (app, wid, at)
}

/// Window `wid`'s present at `now` with a key or none, on ledger `m`: its rest-wake count.
fn rest_wake_after(
    app: &mut crate::App,
    wid: crate::WindowId,
    m: &Metrics,
    observer: &mut PresentObserver,
    now: u64,
    key: bool,
) -> u64 {
    let ws = app.windows.get_mut(&wid).expect("the window");
    let _ = present_at(m, now, 0, key, &mut ws.present_metrics, observer);
    m.window.input.rest_wake_key.get()
}

#[test]
fn a_key_into_a_focused_resting_window_is_a_rest_wake_key() {
    let key = crate::window_rest::tests::key();
    let m = Metrics::new();
    let mut observer = PresentObserver::default();
    let (mut app, wid, at) = resting_app(true);
    app.note_rest_event(wid, &key, at);
    assert!(app.windows[&wid].present_metrics.rest_woke());
    assert_eq!(
        rest_wake_after(&mut app, wid, &m, &mut observer, T0, true),
        1
    );
    assert!(
        !app.windows[&wid].present_metrics.rest_woke(),
        "its present clears the mark"
    );
    assert_eq!(
        rest_wake_after(&mut app, wid, &m, &mut observer, T0 + 100 * MS, true),
        1,
        "the next key is an awake window's"
    );

    let (mut app, wid, at) = resting_app(true);
    let modifiers = aterm_window::event::WindowEvent::ModifiersChanged(Default::default());
    app.note_rest_event(wid, &modifiers, at);
    app.note_rest_event(wid, &key, at + Duration::from_millis(80));
    assert_eq!(
        rest_wake_after(&mut app, wid, &m, &mut observer, T0 + 200 * MS, true),
        2,
        "a modifier wakes the window, and its key presents first"
    );
    let text = inert_text(&m, 0);
    assert_eq!(text_field(&text, "n_rest_wake_key"), Some("2"), "{text}");
}

#[test]
fn a_focus_wake_a_pointer_wake_s_frame_and_an_awake_key_count_no_rest_wake_key() {
    let key = crate::window_rest::tests::key();
    let m = Metrics::new();
    let mut observer = PresentObserver::default();

    let (mut app, wid, at) = resting_app(false);
    app.note_rest_event(wid, &aterm_window::event::WindowEvent::Focused(true), at);
    assert!(!app.windows[&wid].rest.resting(), "the focus wakes it");
    app.windows.get_mut(&wid).expect("the window").focused = true;
    app.note_rest_event(wid, &key, at + Duration::from_millis(80));
    assert_eq!(
        rest_wake_after(&mut app, wid, &m, &mut observer, T0, true),
        0,
        "an unfocused window's deck was still already: its focus wake is no rest wake"
    );

    let (mut app, wid, at) = resting_app(true);
    let pointer = aterm_window::event::WindowEvent::CursorMoved {
        device_id: aterm_window::event::DeviceId::dummy(),
        position: (4.0, 2.0).into(),
    };
    app.note_rest_event(wid, &pointer, at);
    assert!(app.windows[&wid].present_metrics.rest_woke());
    assert_eq!(
        rest_wake_after(&mut app, wid, &m, &mut observer, T0 + 100 * MS, false),
        0,
        "the reading's frame books no input"
    );
    assert!(
        !app.windows[&wid].present_metrics.rest_woke(),
        "and clears the mark"
    );
    app.note_rest_event(wid, &key, at + Duration::from_millis(300));
    assert_eq!(
        rest_wake_after(&mut app, wid, &m, &mut observer, T0 + 400 * MS, true),
        0,
        "the key after that frame meets a warm window"
    );

    let (mut app, wid, at) = resting_app(true);
    app.note_rest_event(wid, &key, at);
    let _ = rest_wake_after(&mut app, wid, &m, &mut observer, T0 + 500 * MS, false);
    app.note_rest_event(wid, &key, at + Duration::from_secs(1));
    assert!(
        !app.windows[&wid].present_metrics.rest_woke(),
        "a key into an awake window marks nothing"
    );
    assert_eq!(
        rest_wake_after(&mut app, wid, &m, &mut observer, T0 + 1_500 * MS, true),
        0
    );
}

/// A ledger whose process presented at `T0`, and a window a rest wake has marked since.
fn woken_after_a_present() -> (Metrics, WindowPresentState, PresentObserver) {
    let m = Metrics::new();
    let mut window = WindowPresentState::default();
    let mut observer = PresentObserver::default();
    let _ = present_at(&m, T0, 0, false, &mut window, &mut observer);
    window.note_rest_wake();
    (m, window, observer)
}

fn counts(m: &Metrics) -> (u64, u64, u64) {
    (
        m.window.input.input.copy_out().n(),
        cold(m),
        m.window.input.rest_wake_key.get(),
    )
}

#[test]
fn a_native_page_s_key_and_a_lens_key_book_as_the_terminal_s_keys_of_their_present() {
    let (m, mut window, mut observer) = woken_after_a_present();
    let now = T0 + 3_000 * MS;
    let input = PresentInput::of(
        &observer,
        Some(&window),
        PresentClock::at(false, now, 0, 0, 0),
    );

    let native = &mut window.pending_native_input.edges;
    m.note_native_window_input(native, now - 2 * MS, now - 2 * MS);
    m.record_native_content_present(native, input.at(now));
    assert_eq!(
        counts(&m),
        (1, 1, 1),
        "a native page's key in a cold present of a woken window is cold and a rest-wake key"
    );

    let mut lens = PendingChromeInputStamp::default();
    let token = m
        .arm_chrome_input_at(&mut lens, now - MS, now - MS)
        .expect("a fresh receipt");
    let clock = PresentClock::at(false, now, 0, 0, 0);
    assert_eq!(m.record_chrome_input_at(&mut lens, token, clock, input), 1);
    assert_eq!(
        counts(&m),
        (2, 2, 2),
        "the stats lens's key in the same present counts as the native page's"
    );

    let _ = present_at(&m, now, 0, false, &mut window, &mut observer);
    assert!(
        !window.rest_woke(),
        "the window's own present clears the mark"
    );
    let warm = PresentInput::of(
        &observer,
        Some(&window),
        PresentClock::at(false, now + 100 * MS, 0, 0, 0),
    )
    .at(now + 100 * MS);
    assert!(!warm.cold && !warm.rest_woke, "{warm:?}");
}

#[test]
fn a_prediction_closed_slice_books_cold_and_as_a_rest_wake_key() {
    let (m, mut window, mut observer) = woken_after_a_present();
    let now = T0 + 3_000 * MS;
    assert!(window.pending_input.arm(now - 2 * MS, now - 2 * MS));
    window.pending_input.tag_newest(Some(7));
    note_prediction_shown(&mut window, Some(7), |_| true);
    assert_eq!(
        present_at(&m, now, 0, false, &mut window, &mut observer),
        1,
        "the frame that shows the prediction closes the key"
    );
    assert_eq!(m.window.input.predicted.get(), 1);
    assert_eq!(
        counts(&m),
        (1, 1, 1),
        "drain_shown books as drain_into does"
    );
}

#[test]
fn a_session_s_input_books_cold_and_never_as_a_rest_wake_key() {
    let (m, mut window, mut observer) = woken_after_a_present();
    let now = T0 + 3_000 * MS;
    m.window.input.session_stamp.arm(now - 2 * MS);
    assert_eq!(
        present_at(&m, now, 0, true, &mut window, &mut observer),
        1,
        "the window's own key"
    );
    assert_eq!(
        counts(&m),
        (2, 2, 1),
        "the session's slice is cold, and names no window to be a rest-wake key"
    );
}

#[test]
fn a_single_present_keeps_its_cold_classification_across_the_threshold() {
    let (m, mut window, mut observer) = woken_after_a_present();
    let before = T0 + COLD_GAP_NS - 1;
    let input = PresentInput::of(
        &observer,
        Some(&window),
        PresentClock::at(false, before, 0, 0, 0),
    );
    let native = &mut window.pending_native_input.edges;
    m.note_native_window_input(native, before - MS, before - MS);
    m.record_native_content_present(native, input.at(before));
    assert_eq!(counts(&m), (1, 0, 1));

    let mut lens = PendingChromeInputStamp::default();
    let token = m
        .arm_chrome_input_at(&mut lens, before - MS, before - MS)
        .unwrap();
    let after = before + 1;
    let clock = PresentClock::at(false, after, 0, 0, 0);
    assert_eq!(m.record_chrome_input_at(&mut lens, token, clock, input), 1);
    assert_eq!(
        counts(&m),
        (2, 0, 2),
        "the same present cannot become cold between its input bookings"
    );
    assert!(window.pending_input.arm(before - MS, before - MS));
    let booking = m.record_present_at(input.clock, MS, MS, Some(&mut window), &mut observer);
    assert_eq!(booking.input_booked, 1);
    assert_eq!(
        counts(&m),
        (3, 0, 3),
        "terminal input uses that same present clock"
    );
}

#[test]
fn a_session_input_after_the_present_clock_waits_for_a_later_present() {
    let m = Metrics::new();
    let mut window = WindowPresentState::default();
    let mut observer = PresentObserver::default();
    let _ = present_at(&m, T0, 0, false, &mut window, &mut observer);
    let output_present = |now, window: &mut WindowPresentState, observer: &mut PresentObserver| {
        m.record_present_at(
            PresentClock::at(false, now, 0, 0, 0),
            MS,
            MS,
            Some(window),
            observer,
        );
    };
    let now = T0 + COLD_GAP_NS;
    m.note_session_input(now + 1);
    output_present(now, &mut window, &mut observer);
    assert_eq!(
        counts(&m),
        (0, 0, 0),
        "an arrival after this present is not its input"
    );
    output_present(now + MS, &mut window, &mut observer);
    assert_eq!(
        counts(&m),
        (1, 0, 0),
        "the later warm present books the pending arrival once"
    );
    assert_eq!(m.window.input.input.last(), MS - 1);
    output_present(now + 2 * MS, &mut window, &mut observer);
    assert_eq!(counts(&m), (1, 0, 0));
}
