// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use super::{CursorStyle, blink_clock_armed, blink_clock_may_run};
use std::time::{Duration, Instant};

/// A resting window runs no blink clock: the caret stays on its visible phase until the
/// window wakes. Only a live, focused, awake window may run one.
#[test]
fn a_resting_window_runs_no_blink_clock() {
    assert!(blink_clock_may_run(true, true, false));
    assert!(!blink_clock_may_run(true, true, true), "resting");
    assert!(
        !blink_clock_may_run(false, true, false),
        "headless or no window"
    );
    assert!(!blink_clock_may_run(true, false, false), "unfocused");
}

/// The timer's step flips an awake window's due blink and arms the next; a resting window's
/// due blink keeps its phase and its deadline, which the about-to-wait fold then clears.
#[test]
fn a_resting_window_never_flips_its_due_blink() {
    let mut app = crate::App::headless_for_test();
    // Rest is for a window on screen: a headless instance's never rests. It is off unless set.
    app.headless = false;
    app.config.idle_rest_secs = Some(30.0);
    let wid = crate::WindowId(0);
    let t0 = Instant::now();
    let ws = app.windows.get_mut(&wid).expect("the window");
    ws.next_blink = Some(t0 + Duration::from_millis(1));
    assert!(!ws.flip_due_blink(t0), "not yet due");
    let phase = ws.blink_phase;
    assert!(ws.flip_due_blink(t0 + Duration::from_millis(1)), "due");
    assert_eq!(ws.blink_phase, !phase);
    assert_eq!(
        ws.next_blink,
        Some(t0 + Duration::from_millis(1) + super::BLINK_INTERVAL)
    );

    let rest_at = t0 + Duration::from_secs(31);
    app.settle_rest(rest_at);
    let ws = app.windows.get_mut(&wid).expect("the window");
    assert!(ws.rest.resting());
    ws.next_blink = Some(rest_at);
    let phase = ws.blink_phase;
    assert!(
        !ws.flip_due_blink(rest_at),
        "a resting window's blink is due and never flips"
    );
    assert_eq!(ws.blink_phase, phase);
    assert_eq!(ws.next_blink, Some(rest_at));
}

#[test]
fn under_the_rainbow_the_blink_clock_is_never_armed() {
    assert!(!blink_clock_armed(true, CursorStyle::BlinkingBlock, true));
    assert!(blink_clock_armed(true, CursorStyle::BlinkingBar, true));
    assert!(blink_clock_armed(
        true,
        CursorStyle::BlinkingUnderline,
        true
    ));
}

#[test]
fn under_a_classic_style_the_blink_clock_follows_the_terminal() {
    assert!(blink_clock_armed(true, CursorStyle::BlinkingBlock, false));
    assert!(blink_clock_armed(true, CursorStyle::BlinkingBar, false));
    assert!(!blink_clock_armed(true, CursorStyle::SteadyBlock, false));
    assert!(
        !blink_clock_armed(false, CursorStyle::BlinkingBlock, false),
        "hidden: no clock"
    );
}
