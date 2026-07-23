// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Tier-1 binding between the derived predictive-visibility model and the
//! genuine `aterm-predict` state machine. The test drives the shipping clocked
//! implementation, projects its user-visible decisions, and requires the same
//! transitions as `predictive_echo_visibility_model`.

use std::time::{Duration, Instant};

use aterm_predict::{PredictMode, Predictor};

fn echoed_a(row: u16, col: u16) -> Option<char> {
    ((row, col) == (0, 0)).then_some('a')
}

#[test]
fn adaptive_fast_expiry_is_invisible_and_slow_echo_may_display() {
    let model = aterm_spec::derive::predictive_echo_visibility_model();
    let now = Instant::now();

    // Fast-link path: the real implementation records the RTT and tracks the
    // next guess, but its overlay is empty both before and across expiry.
    let mut fast_model = model.init_state();
    let mut fast = Predictor::new(PredictMode::Adaptive);
    assert!(fast.predict_char('a', (0, 0), 80, now));
    let fast_echo = now + Duration::from_millis(1);
    fast.reconcile(Some((0, 1)), false, fast_echo, echoed_a);
    assert!(model.fire("ConfirmFast", &mut fast_model));
    assert!(fast.predict_char('b', (0, 1), 80, fast_echo));
    assert!(model.fire("Key", &mut fast_model));
    assert_eq!(fast_model["pending"], 1);
    assert_eq!(fast_model["visible"], 0);
    assert!(fast.overlay(fast_echo).is_empty());

    // Negative control: the genuine predictor has a tracked, deadline-bearing `b`
    // after a real confirmation, while the shipping benefit gate keeps its overlay
    // empty. The deleted confirmation-only gate would expose that exact tracked
    // glyph. Derive both operands from the shipping predictor/model transition so
    // this control fails if tracking, confirmation, or the real overlay drifts.
    let tracked_fast_prediction = fast.next_deadline().is_some();
    assert!(
        tracked_fast_prediction,
        "shipping Predictor tracks fast-link `b`"
    );
    assert_eq!(fast_model["confirmed"], 1);
    let confirmation_only_visible =
        usize::from(tracked_fast_prediction && fast_model["confirmed"] == 1);
    assert_ne!(
        confirmation_only_visible,
        fast.overlay(fast_echo).len(),
        "the deleted confirmation-only gate must disagree with shipping pixels"
    );

    let expired = fast_echo + Duration::from_millis(251);
    assert!(model.fire("Expire", &mut fast_model));
    assert_eq!(fast_model["erased"], 0);
    assert!(fast.overlay(expired).is_empty());
    assert!(fast.idle());

    // Slow-link control: Adaptive remains useful. The model's `ConfirmSlow`
    // abstracts the shipping classifier's stable-slow transition, which requires
    // two consecutive 60 ms confirmations so one scheduler tail cannot paint.
    let mut slow_model = model.init_state();
    let mut slow = Predictor::new(PredictMode::Adaptive);
    assert!(slow.predict_char('a', (0, 0), 80, now));
    let slow_echo_1 = now + Duration::from_millis(60);
    slow.reconcile(Some((0, 1)), false, slow_echo_1, echoed_a);
    assert!(slow.predict_char('b', (0, 1), 80, slow_echo_1));
    assert!(slow.overlay(slow_echo_1).is_empty());
    let slow_echo = slow_echo_1 + Duration::from_millis(60);
    slow.reconcile(Some((0, 2)), false, slow_echo, |row, col| {
        ((row, col) == (0, 1)).then_some('b')
    });
    assert!(model.fire("ConfirmSlow", &mut slow_model));
    assert!(slow.predict_char('c', (0, 2), 80, slow_echo));
    assert!(model.fire("Key", &mut slow_model));
    assert_eq!(slow_model["visible"], 1);
    assert_eq!(slow.overlay(slow_echo).len(), 1);

    // Session switch: the model and genuine Predictor both discard that slow
    // link's eligibility. A 1 ms confirmation in the new pane stays invisible.
    slow.reset_session();
    assert!(model.fire("SwitchSession", &mut slow_model));
    assert_eq!(slow_model["fresh"], 1);
    assert_eq!(slow_model["slow"], 0);
    let local_start = slow_echo + Duration::from_millis(1);
    assert!(slow.predict_char('x', (0, 0), 80, slow_echo));
    slow.reconcile(Some((0, 1)), false, local_start, |row, col| {
        ((row, col) == (0, 0)).then_some('x')
    });
    assert!(model.fire("ConfirmFast", &mut slow_model));
    assert!(slow.predict_char('y', (0, 1), 80, local_start));
    assert!(model.fire("Key", &mut slow_model));
    assert_eq!(slow_model["visible"], 0);
    assert!(slow.overlay(local_start).is_empty());

    // Negative control: the old window-global EWMA survives the switch and is
    // explicitly rejected by the model's fresh-session invariant.
    let mut inherited = model.init_state();
    inherited.insert("slow", 1);
    assert!(!model.check_invariant("FreshSessionHasNoInheritedRtt", &inherited));
}
