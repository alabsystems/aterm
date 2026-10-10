// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Tier-1 binding between the derived predictive-visibility model and the
//! genuine `aterm-predict` state machine. The test drives the shipping clocked
//! implementation, projects its user-visible decisions, and requires the same
//! transitions as `predictive_echo_visibility_model`.

use std::time::{Duration, Instant};

use aterm_predict::{PredictMode, Predictor};

/// The content-coordinate generation of a screen whose rows never move.
const G: u64 = 0;

fn echoed_a(row: u16, col: u16) -> Option<char> {
    ((row, col) == (0, 0)).then_some('a')
}

/// One complete DRAINED type→echo turn on row 0: arm `ch` at `col`, then confirm it
/// `rtt` later with the real cursor advanced past it. Returns the confirmation
/// instant. Only valid with an empty pending set (it anchors on the passed cursor).
fn echo_turn(p: &mut Predictor, ch: char, col: u16, at: Instant, rtt: Duration) -> Instant {
    assert!(p.predict_char(ch, (0, col), G, 80, at));
    let done = at + rtt;
    p.reconcile(Some((0, col + 1)), G, false, done, move |row, c| {
        ((row, c) == (0, col)).then_some(ch)
    });
    done
}

#[test]
fn adaptive_fast_expiry_is_invisible_and_slow_echo_may_display() {
    let model = aterm_spec::derive::predictive_echo_visibility_model();
    let now = Instant::now();

    // Fast-link path: the real implementation records the RTT and tracks the
    // next guess, but its overlay is empty both before and across expiry.
    let mut fast_model = model.init_state();
    let mut fast = Predictor::new(PredictMode::Adaptive);
    assert!(fast.predict_char('a', (0, 0), G, 80, now));
    let fast_echo = now + Duration::from_millis(1);
    fast.reconcile(Some((0, 1)), G, false, fast_echo, echoed_a);
    assert!(model.fire("ConfirmFast", &mut fast_model));
    assert!(fast.predict_char('b', (0, 1), G, 80, fast_echo));
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
    assert!(slow.predict_char('a', (0, 0), G, 80, now));
    let slow_echo_1 = now + Duration::from_millis(60);
    slow.reconcile(Some((0, 1)), G, false, slow_echo_1, echoed_a);
    assert!(slow.predict_char('b', (0, 1), G, 80, slow_echo_1));
    assert!(slow.overlay(slow_echo_1).is_empty());
    let slow_echo = slow_echo_1 + Duration::from_millis(60);
    slow.reconcile(Some((0, 2)), G, false, slow_echo, |row, col| {
        ((row, col) == (0, 1)).then_some('b')
    });
    assert!(model.fire("ConfirmSlow", &mut slow_model));
    assert!(slow.predict_char('c', (0, 2), G, 80, slow_echo));
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
    assert!(slow.predict_char('x', (0, 0), G, 80, slow_echo));
    slow.reconcile(Some((0, 1)), G, false, local_start, |row, col| {
        ((row, col) == (0, 0)).then_some('x')
    });
    assert!(model.fire("ConfirmFast", &mut slow_model));
    assert!(slow.predict_char('y', (0, 1), G, 80, local_start));
    assert!(model.fire("Key", &mut slow_model));
    assert_eq!(slow_model["visible"], 0);
    assert!(slow.overlay(local_start).is_empty());

    // Negative control: the old window-global EWMA survives the switch and is
    // explicitly rejected by the model's fresh-session invariant.
    let mut inherited = model.init_state();
    inherited.insert("slow", 1);
    assert!(!model.check_invariant("FreshSessionHasNoInheritedRtt", &inherited));
}

/// The gate is a LATCH: it stops painting only after a SUSTAINED fast run with
/// nothing in flight. This drives the sequence a single-sample close cannot
/// survive, on BOTH sides (the binding above only fires `ConfirmFast` at an
/// already-closed latch, so it cannot pin this).
#[test]
fn the_display_gate_closes_only_on_sustained_fast_evidence() {
    let model = aterm_spec::derive::predictive_echo_visibility_model();
    let mut st = model.init_state();
    let mut p = Predictor::new(PredictMode::Adaptive);
    let now = Instant::now();
    let slow = Duration::from_millis(60);
    let fast = Duration::from_millis(1);

    // Two consecutive 60 ms confirmations are the shipping classifier's stable-slow
    // transition, which the model folds into one `ConfirmSlow`.
    let mut at = echo_turn(&mut p, 'a', 0, now, slow);
    at = echo_turn(&mut p, 'b', 1, at, slow);
    assert!(model.fire("ConfirmSlow", &mut st));
    assert!(p.predict_char('c', (0, 2), G, 80, at));
    assert!(model.fire("Key", &mut st));
    assert_eq!(st["visible"], 1);
    assert_eq!(p.overlay(at).len(), 1, "control: the slow link paints");

    // A fast turn taken while type-ahead is still on glass. Closing HERE is the blink
    // itself — the very turn that decides speculation is unnecessary is the turn that
    // un-paints the glyph the user is looking at.
    assert!(p.predict_char('d', (0, 3), G, 80, at));
    at += fast;
    p.reconcile(Some((0, 3)), G, false, at, |row, col| {
        ((row, col) == (0, 2)).then_some('c')
    });
    assert!(model.fire("ConfirmFastInFlight", &mut st));
    assert_eq!(
        st["slow"], 1,
        "a gate flip with pixels in flight is the blink"
    );
    assert_eq!(st["retracted"], 0);
    assert_eq!(st["visible"], 1);
    assert_eq!(
        p.overlay(at).len(),
        1,
        "the shipping gate keeps painting 'd'"
    );

    // Drain, then one DECISIVE fast turn with nothing pending: still open. The model
    // counts turns; the implementation additionally waits for its smoothed estimate to
    // agree, so the two are bound at the DECISIONS (open / still open / closed), not
    // turn for turn — the same abstraction `ConfirmSlow` already makes.
    at += fast;
    p.reconcile(Some((0, 4)), G, false, at, |row, col| {
        ((row, col) == (0, 3)).then_some('d')
    });
    assert!(model.fire("Echo", &mut st));
    at = echo_turn(&mut p, 'e', 4, at, fast);
    assert!(model.fire("ConfirmFast", &mut st));
    assert_eq!(
        st["slow"], 1,
        "one decisive fast turn must not close the gate"
    );
    assert_eq!(st["retracted"], 0);
    assert!(p.predict_char('z', (0, 5), G, 80, at));
    assert!(model.fire("Key", &mut st));
    assert_eq!(st["visible"], 1);
    assert_eq!(
        p.overlay(at).len(),
        1,
        "…and the shipping gate is still painting too"
    );

    // Keep echoing locally until the implementation's EWMA agrees as well. Only then
    // does the third decisive turn retire the latch on both sides.
    at += fast;
    p.reconcile(Some((0, 6)), G, false, at, |row, col| {
        ((row, col) == (0, 5)).then_some('z')
    });
    assert!(model.fire("Echo", &mut st));
    for col in 6..10 {
        at = echo_turn(&mut p, 'x', col, at, fast);
    }
    assert!(model.fire("ConfirmFast", &mut st));
    assert_eq!(st["slow"], 0, "a sustained fast run finally closes it");
    assert_eq!(st["retracted"], 1, "…and that step is the retraction");
    assert!(model.check_invariant("RetractOnlyOnSustainedFastEvidence", &st));

    assert!(p.predict_char('q', (0, 10), G, 80, at));
    assert!(model.fire("Key", &mut st));
    assert_eq!(st["visible"], 0);
    assert!(
        p.overlay(at).is_empty(),
        "a settled local link stops painting on both sides"
    );

    // Tier-0 negative controls for the retraction rule: the deleted single-sample
    // close, and a close taken with pixels still in flight, are both rejected.
    let mut single_sample = model.init_state();
    single_sample.insert("retracted", 1);
    single_sample.insert("fast_streak", 1);
    assert!(
        !model.check_invariant("RetractOnlyOnSustainedFastEvidence", &single_sample),
        "one fast sample is not evidence enough to un-paint a ghost"
    );

    let mut closed_in_flight = model.init_state();
    closed_in_flight.insert("retracted", 1);
    closed_in_flight.insert("fast_streak", 3);
    closed_in_flight.insert("pending", 1);
    assert!(
        !model.check_invariant("RetractOnlyOnSustainedFastEvidence", &closed_in_flight),
        "even a full streak may not close the gate over pixels in flight"
    );
}

/// Every epoch boundary the shipping predictor recognises is the model's `Submit`
/// (`confirmed = 0`): the proof belongs to the epoch that ended, so the next `Key`
/// is invisible on both sides. A boundary missed is a proven epoch carried into a
/// line that never echoed — the password-prompt leak.
#[test]
fn every_epoch_boundary_is_the_models_submit() {
    // Each boundary returns the generation and the cursor column the next key
    // arrives with.
    type Boundary = fn(&mut Predictor, Instant) -> (u64, u16);
    let boundaries: [(&str, Boundary); 6] = [
        ("a line terminator", |p, _| {
            p.note_line_submit();
            (G, 2)
        }),
        ("any other key the host could not predict", |p, _| {
            p.note_unmodelled_input();
            (G, 2)
        }),
        ("a Backspace with nothing of ours to cancel", |p, at| {
            assert!(!p.predict_backspace(at));
            (G, 2)
        }),
        ("a glyph the predictor declines to place", |p, at| {
            assert!(!p.predict_char('日', (0, 2), G, 80, at));
            (G, 2)
        }),
        // No call at all: the generation handed with the next key moved.
        ("the rows moved under the cursor", |_, _| (G + 1, 2)),
        // No call at all: the program printed a prompt beside the echo.
        (
            "the program moved the cursor off the proven cell",
            |_, _| (G, 14),
        ),
    ];
    let model = aterm_spec::derive::predictive_echo_visibility_model();
    let slow = Duration::from_millis(60);
    for (label, boundary) in boundaries {
        let mut st = model.init_state();
        let mut p = Predictor::new(PredictMode::Adaptive);
        let mut at = echo_turn(&mut p, 'a', 0, Instant::now(), slow);
        at = echo_turn(&mut p, 'b', 1, at, slow);
        assert!(model.fire("ConfirmSlow", &mut st));
        let (coords, col) = boundary(&mut p, at);
        assert!(model.fire("Submit", &mut st));
        assert_eq!(st["confirmed"], 0);
        assert!(p.predict_char('s', (0, col), coords, 80, at), "{label}");
        assert!(model.fire("Key", &mut st));
        assert_eq!(st["visible"], 0);
        assert!(
            p.overlay(at).is_empty(),
            "{label}: the next keystroke painted"
        );
    }

    // Negative control: the state a missed boundary produces (a guess displayed on
    // a proof the epoch never earned) is exactly what the model rejects.
    let mut leaked = model.init_state();
    leaked.insert("slow", 1);
    leaked.insert("fresh", 0);
    leaked.insert("pending", 1);
    leaked.insert("visible", 1);
    assert!(!model.check_invariant("VisibleNeedsProofAndBenefit", &leaked));
}
