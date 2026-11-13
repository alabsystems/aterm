// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use super::{
    INPUT_SLICE_CAP_NS, Metrics, PresentBooking, PresentClock, PresentObserver, WindowPresentState,
    now_ns,
};

const MS: u64 = 1_000_000;

fn present(
    m: &Metrics,
    now: u64,
    epoch: u64,
    latency_ns: u64,
    window: &mut WindowPresentState,
    observer: &mut PresentObserver,
) -> PresentBooking {
    let clock = PresentClock {
        now,
        epoch,
        capture_forced: false,
        tainted: false,
    };
    m.record_present_at(clock, latency_ns, MS, Some(window), observer)
}

#[test]
fn another_windows_present_cannot_hide_this_windows_gap() {
    let m = Metrics::new();
    let t0 = 10_000 * MS;
    let stall = t0 + 500 * MS;
    let mut a = WindowPresentState::default();
    let mut b = WindowPresentState::default();
    let mut observer = PresentObserver::default();
    let first = present(&m, t0, 0, 0, &mut a, &mut observer);
    assert_eq!(
        first.window_gap_ns, None,
        "a window's first present has no gap"
    );
    let mut t = t0;
    while t + 16 * MS < stall {
        t += 16 * MS;
        let streamed = present(&m, t, 0, 0, &mut b, &mut observer);
        assert_eq!(streamed.frame_gap_ns, Some(16 * MS), "at {t}");
    }
    let stalled = present(&m, stall, 0, 0, &mut a, &mut observer);
    assert_eq!(
        stalled.window_gap_ns,
        Some(500 * MS),
        "window A's 500 ms stall is its witness gap"
    );
    assert_eq!(
        stalled.frame_gap_ns,
        Some(stall - t),
        "the process-wide cadence still counts B's frames"
    );

    let mut shared = WindowPresentState::default();
    let mut observer = PresentObserver::default();
    let _ = present(&m, t0, 0, 0, &mut shared, &mut observer);
    let mut t = t0;
    while t + 16 * MS < stall {
        t += 16 * MS;
        let _ = present(&m, t, 0, 0, &mut shared, &mut observer);
    }
    let hidden = present(&m, stall, 0, 0, &mut shared, &mut observer);
    assert_eq!(
        hidden.window_gap_ns,
        Some(stall - t),
        "one shared stamp reads B's last frame, not A's stall"
    );
}

#[test]
fn the_first_present_after_a_reset_books_no_gap() {
    let m = Metrics::new();
    let t = 10_000 * MS;
    let mut window = WindowPresentState::default();
    let mut observer = PresentObserver::default();
    let _ = present(&m, t, 0, 0, &mut window, &mut observer);
    let first = present(&m, t + 10 * MS, t + 1, 0, &mut window, &mut observer);
    assert_eq!((first.window_gap_ns, first.frame_gap_ns), (None, None));
    let second = present(&m, t + 20 * MS, t + 1, 0, &mut window, &mut observer);
    assert_eq!(
        (second.window_gap_ns, second.frame_gap_ns),
        (Some(10 * MS), Some(10 * MS)),
        "the post-reset present is the next one's previous present"
    );

    let mut window = WindowPresentState::default();
    let mut observer = PresentObserver::default();
    let _ = present(&m, t, 0, 0, &mut window, &mut observer);
    let unreset = present(&m, t + 10 * MS, 0, 0, &mut window, &mut observer);
    assert_eq!(
        (unreset.window_gap_ns, unreset.frame_gap_ns),
        (Some(10 * MS), Some(10 * MS))
    );

    let before = now_ns();
    m.reset();
    assert!(PresentClock::read(&m, false).epoch >= before);
}

#[test]
fn the_taint_decision_is_made_once() {
    let m = Metrics::new();
    let until = 20_000 * MS;
    let typed = until - 50 * MS;
    let at = |now: u64| {
        let mut window = WindowPresentState::default();
        assert!(window.pending_input.arm(typed, typed));
        let clock = PresentClock::at(false, now, 0, until, 0);
        let mut observer = PresentObserver::default();
        let booked = m.record_present_at(clock, MS, MS, Some(&mut window), &mut observer);
        (booked.output_tainted, booked.input_booked)
    };
    assert_eq!(
        at(until - 1),
        (Some(true), 1),
        "inside the tail: the tainted ledger"
    );
    assert_eq!(
        at(until),
        (Some(false), 1),
        "the tail is exclusive: on glass"
    );
}

#[test]
fn a_forced_present_books_a_tainted_capture_present() {
    let m = Metrics::new();
    let now = 20_000 * MS;
    let typed = now - 50 * MS;
    let book = |capture_forced: bool| {
        let mut window = WindowPresentState::default();
        assert!(window.pending_input.arm(typed, typed));
        let clock = PresentClock::at(capture_forced, now, 0, 0, 0);
        let mut observer = PresentObserver::default();
        let booked = m.record_present_at(clock, MS, MS, Some(&mut window), &mut observer);
        booked.output_tainted
    };
    assert_eq!(book(true), Some(true), "forced: tainted");
    let present = &m.window.present;
    assert_eq!(present.capture_presents.get(), 1);
    assert_eq!(present.tainted_samples.get(), 1);
    assert_eq!(book(false), Some(false), "unforced: on glass");
    assert_eq!(
        present.capture_presents.get(),
        1,
        "an unforced present is no capture_presents sample"
    );
    assert_eq!(present.tainted_samples.get(), 1);
}

#[test]
fn a_key_typed_before_a_reset_is_drained_not_booked() {
    let m = Metrics::new();
    let reset_at = 10_000 * MS;
    let before = reset_at - 20 * MS;
    let after = reset_at + 30 * MS;
    let echo = reset_at + 100 * MS;
    let present_keys = |keys: &[u64], epoch: u64| {
        let mut window = WindowPresentState::default();
        for &key in keys {
            assert!(window.pending_input.arm(key, key));
        }
        let mut observer = PresentObserver::default();
        let booked = present(&m, echo, epoch, MS, &mut window, &mut observer);
        assert!(
            !window.pending_input.is_pending(),
            "the present drains every key"
        );
        booked.input_booked
    };
    assert_eq!(
        present_keys(&[before], reset_at),
        0,
        "the lone key typed before the reset books nothing"
    );
    assert_eq!(
        present_keys(&[before, after], reset_at),
        1,
        "only the key armed after the reset books"
    );
    assert_eq!(present_keys(&[before, after], 0), 2);
    let aged = echo - INPUT_SLICE_CAP_NS - MS;
    assert_eq!(present_keys(&[aged], reset_at), 0);
    assert_eq!(present_keys(&[aged], 0), 0);
}

#[test]
fn a_present_that_shows_a_keys_prediction_closes_its_slice() {
    let m = Metrics::new();
    let t = 10_000 * MS;
    let mut window = WindowPresentState::default();
    for (i, op) in [Some(5), Some(6), None, Some(7)].into_iter().enumerate() {
        let key = t + i as u64 * 10 * MS;
        assert!(window.pending_input.arm(key, key));
        if let Some(op) = op {
            window.pending_input.tag_newest(Some(op));
        }
    }
    let mut observer = PresentObserver::default();
    let blink = present(&m, t + 50 * MS, 0, 0, &mut window, &mut observer);
    assert_eq!(blink.input_booked, 0, "no prediction shown: nothing booked");
    super::note_prediction_shown(&mut window, Some(7), |_| true);
    let shown = present(&m, t + 60 * MS, 0, 0, &mut window, &mut observer);
    assert_eq!(
        shown.input_booked, 2,
        "the keys of ops 5 and 6; the key that armed no op stops the run"
    );
    assert_eq!(
        (shown.input_predicted, m.window.input.predicted.get()),
        (2, 2),
        "both counted as closed by the prediction"
    );
    assert!(window.pending_input.is_pending());
    let after = present(&m, t + 70 * MS, 0, 0, &mut window, &mut observer);
    assert_eq!(after.input_booked, 0, "the mark was the one frame's");
    let echo = present(&m, t + 200 * MS, 0, MS, &mut window, &mut observer);
    assert_eq!(
        echo.input_booked, 2,
        "the echo's content present books the rest"
    );
    assert_eq!(
        (echo.input_predicted, m.window.input.predicted.get()),
        (0, 2),
        "a content present's keys are not counted as predicted"
    );
    assert!(!window.pending_input.is_pending());
}

#[test]
fn a_dropped_key_tags_no_other_keys_edge() {
    let m = Metrics::new();
    let t = 10_000 * MS;
    let mut window = WindowPresentState::default();
    let ring = super::INPUT_EDGE_RING as u64;
    for i in 0..ring {
        assert!(window.pending_input.arm(t + i, t + i));
        window.pending_input.tag_newest((i + 1 < ring).then_some(i));
    }
    assert!(
        !window.pending_input.arm(t + 100, t + 100),
        "the ring is full"
    );
    window.pending_input.tag_newest(Some(ring));
    super::note_prediction_shown(&mut window, Some(ring), |_| true);
    let mut observer = PresentObserver::default();
    let shown = present(&m, t + 50 * MS, 0, 0, &mut window, &mut observer);
    assert_eq!(
        shown.input_booked,
        u32::try_from(super::INPUT_EDGE_RING - 1).expect("a small ring"),
        "every key the frame shows; the ring's newest edge is not the dropped key's"
    );
    assert!(
        window.pending_input.is_pending(),
        "the newest key waits for its echo"
    );
}
