// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Tier-1 binding between the derived predictive-visibility model and the
//! genuine `aterm-predict` state machine. The test drives the shipping clocked
//! implementation, projects its user-visible decisions, and requires the same
//! transitions as `predictive_echo_visibility_model`.

use std::time::{Duration, Instant};

use aterm_predict::{EditKey, EditOutcome, KeySample, PredictMode, Predictor, Rule, Seen};

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
    let model = aterm_models_core::predictive_echo_visibility_model();
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
    // The expiry owes `b`'s echo and voids the proof `a` earned (both sides; the
    // shipping side is pinned through the pixels in
    // `expiry_owes_and_voids_the_proof`, since a fast link paints nothing here).
    assert_eq!((fast_model["owed"], fast_model["confirmed"]), (1, 0));
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
    let model = aterm_models_core::predictive_echo_visibility_model();
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
    let boundaries: [(&str, Boundary); 7] = [
        ("a line terminator", |p, at| {
            p.note_line_submit(at);
            (G, 2)
        }),
        ("any other key the host could not predict", |p, at| {
            p.note_unmodelled_input(at);
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
        // An arrow the edit lane cannot place (here behind a host that
        // reconciles through the legacy observe): declined with its pixels kept,
        // owed, the epoch ended.
        ("an arrow the predictor cannot place", |p, at| {
            let row = vec![Seen::Blank; 80];
            let sample = KeySample {
                cursor: (0, 2),
                coords: G,
                grid: (80, 24),
                row: Some(&row),
            };
            assert_eq!(
                p.predict_edit(EditKey::Left, &sample, at),
                EditOutcome::Declined
            );
            (G, 2)
        }),
    ];
    let model = aterm_models_core::predictive_echo_visibility_model();
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

/// The shipping predictor through the owed-echo scenario: a slow proven line, `n`
/// typed and painted, the program moving the cursor to the next row 10 ms later
/// (the guess is flushed while its keystroke is in flight), `n` typed again at the
/// new cursor `flushed_age` after the first, and the FIRST `n`'s echo landing in
/// the new guess's cell 60 ms later. Returns the predictor, the instant, and
/// whether the space typed next is painted, which is the shipping proof read
/// through the pixels.
fn owed_echo_run(flushed_age: Duration) -> (Predictor, Instant, bool) {
    let mut p = Predictor::new(PredictMode::Adaptive);
    let slow = Duration::from_millis(60);
    let mut at = echo_turn(&mut p, 'a', 0, Instant::now(), slow);
    at = echo_turn(&mut p, 'b', 1, at, slow);
    assert!(p.predict_char('n', (0, 2), G, 80, at));
    assert_eq!(
        p.overlay(at).len(),
        1,
        "control: the proven line paints 'n'"
    );
    at += Duration::from_millis(10);
    p.reconcile(Some((1, 2)), G, false, at, ab);
    assert!(
        p.idle(),
        "the cursor left 'n': flushed, its echo still owed"
    );
    at += flushed_age.saturating_sub(Duration::from_millis(10));
    assert!(p.predict_char('n', (1, 2), G, 80, at));
    assert!(p.overlay(at).is_empty());
    at += slow;
    p.reconcile(Some((1, 3)), G, false, at, first_echo);
    assert!(
        p.idle(),
        "glyph and cursor advance match: the pixels retire"
    );
    assert!(p.predict_char(' ', (1, 3), G, 80, at));
    let proven = !p.overlay(at).is_empty();
    (p, at, proven)
}

fn ab(row: u16, col: u16) -> Option<char> {
    match (row, col) {
        (0, 0) => Some('a'),
        (0, 1) => Some('b'),
        _ => None,
    }
}

fn first_echo(row: u16, col: u16) -> Option<char> {
    ab(row, col).or(((row, col) == (1, 2)).then_some('n'))
}

/// ATTRIBUTION: an echo still owed to a keystroke no guess tracks proves nothing.
/// The shipping predictor flushes a guess whose keystroke is still in flight (the
/// program moved the cursor: the model's `Submit`, which owes that echo), the same
/// glyph is typed again at the new cursor, and the FIRST keystroke's echo lands in
/// the new guess's cell. Glyph and cursor advance match, the pixels retire, and
/// the epoch stays unproven: the model's `Echo` under `owed = 1`, never a
/// `Confirm*`. Once the owed echo's window has passed (`OwedExpire`), a drained
/// echo turn is `ConfirmSlow` again on both sides.
#[test]
fn an_owed_echo_proves_nothing_until_its_window_passes() {
    let model = aterm_models_core::predictive_echo_visibility_model();
    let slow = Duration::from_millis(60);
    let mut st = model.init_state();
    assert!(model.fire("ConfirmSlow", &mut st));
    assert!(model.fire("Key", &mut st));
    assert_eq!(st["visible"], 1);
    assert!(model.fire("Submit", &mut st));
    assert_eq!((st["confirmed"], st["owed"]), (0, 1));
    assert!(model.fire("Key", &mut st));
    assert!(model.fire("Echo", &mut st));
    assert!(
        !model.fire("ConfirmSlow", &mut st.clone()) && !model.fire("ConfirmFast", &mut st.clone()),
        "no confirm turn is enabled while the echo is owed"
    );
    // The shipping run, the same steps: the new 'n' is typed 10 ms after the
    // first, so the first one's echo is owed when it lands.
    let (mut p, mut at, shipping_proven) = owed_echo_run(Duration::from_millis(10));
    assert!(model.fire("Key", &mut st));
    assert_eq!(st["visible"], 0);
    assert!(
        !shipping_proven,
        "the space echoes after the second 'n', not at (1, 3)"
    );

    // Negative control, from the code: the deleted rule credited every match. The
    // shipping predictor credits one exactly when the flushed key is older than
    // its window, so the same run with the second 'n' typed a window later is
    // that credit, measured — not a constant. Projected onto the model's state at
    // this step (the echo owed), it must be rejected; if the shipping code ever
    // stopped crediting a positional match at all, this run proves nothing and the
    // control fails instead of passing vacuously.
    let (_, _, credited) = owed_echo_run(Duration::from_millis(300));
    let mut borrowed = st.clone();
    borrowed.insert("confirmed", i64::from(credited));
    assert!(
        !model.check_invariant("ProofIsAttributable", &borrowed),
        "a proof borrowed from an owed echo must be rejected (credited: {credited})"
    );

    // The second 'n' echoes; the space diverges and is flushed (owed in turn).
    at += slow;
    let second_echo =
        |row: u16, col: u16| first_echo(row, col).or(((row, col) == (1, 3)).then_some('n'));
    p.reconcile(Some((1, 4)), G, false, at, second_echo);
    assert!(p.idle());
    assert!(model.fire("Submit", &mut st));

    // Past the longest glitch window (the documented 2 s ceiling) nothing owed
    // can still be in flight, and a drained echo turn proves the epoch again.
    at += Duration::from_millis(2001);
    assert!(model.fire("OwedExpire", &mut st));
    assert!(p.predict_char('q', (1, 5), G, 80, at));
    at += slow;
    p.reconcile(Some((1, 6)), G, false, at, |row, col| {
        second_echo(row, col).or(((row, col) == (1, 5)).then_some('q'))
    });
    assert!(model.fire("ConfirmSlow", &mut st));
    assert!(p.predict_char('r', (1, 6), G, 80, at));
    assert!(model.fire("Key", &mut st));
    assert_eq!(st["visible"], 1);
    assert_eq!(
        p.overlay(at).len(),
        1,
        "an attributable echo proves the epoch again"
    );
    assert!(model.check_invariant("ProofIsAttributable", &st));
}

/// The shipping predictor through an expiry: a slow proven line `ab`, `c` typed
/// and painted at the proven cell, no echo within the glitch window, so `c`
/// expires; then `d` is typed at the live cursor, which still stands on `c`'s
/// cell. Returns whether `d` is painted: the shipping proof, read through the
/// pixels. With `rule_off` the predictor runs with `Rule::ExpiryOwes` switched
/// off, the expiry as it shipped before the fix.
fn expiry_run(rule_off: bool) -> bool {
    let mut p = Predictor::new(PredictMode::Adaptive);
    if rule_off {
        p.disable_rule_for_test(Rule::ExpiryOwes);
    }
    let slow = Duration::from_millis(60);
    let mut at = echo_turn(&mut p, 'a', 0, Instant::now(), slow);
    at = echo_turn(&mut p, 'b', 1, at, slow);
    assert!(p.predict_char('c', (0, 2), G, 80, at));
    assert_eq!(
        p.overlay(at).len(),
        1,
        "control: the proven line paints 'c'"
    );
    at += p.glitch_window_for_test() + Duration::from_millis(1);
    assert!(p.overlay(at).is_empty(), "'c' expired");
    assert!(p.predict_char('d', (0, 2), G, 80, at));
    !p.overlay(at).is_empty()
}

/// EXPIRY OWES: an expired guess's key was sent, so its echo is owed, and when
/// it comes it lands on the cell the proof names. The model's `Expire` owes
/// (`owed = 1`) and clears `confirmed`, so the next `Key` is invisible; the
/// shipping predictor leaves `d` dark. Negative control, from the code: the same
/// drive with `Rule::ExpiryOwes` off paints `d`, and that proof, projected onto
/// the model's post-`Expire` state, is what `ProofIsAttributable` rejects; if the
/// mutated code stopped painting, the control would prove nothing and fails
/// instead of passing vacuously.
#[test]
fn expiry_owes_and_voids_the_proof() {
    let model = aterm_models_core::predictive_echo_visibility_model();
    let mut st = model.init_state();
    assert!(model.fire("ConfirmSlow", &mut st));
    assert!(model.fire("Key", &mut st));
    assert_eq!(st["visible"], 1);
    assert!(model.fire("Expire", &mut st));
    assert_eq!((st["owed"], st["confirmed"], st["erased"]), (1, 0, 1));
    let shipping_painted = expiry_run(false);
    let after_expiry = st.clone();
    assert!(model.fire("Key", &mut st));
    assert_eq!(st["visible"], 0);
    assert!(
        !shipping_painted,
        "'d' echoes after 'c''s late echo, one cell on: not painted on 'c''s cell"
    );

    let kept = expiry_run(true);
    assert!(kept, "the mutated expiry keeps the proof and paints 'd'");
    let mut borrowed = after_expiry;
    borrowed.insert("confirmed", i64::from(kept));
    assert!(
        !model.check_invariant("ProofIsAttributable", &borrowed),
        "a proof kept across an expiry must be rejected"
    );
}

/// Row 0 of an 80-column screen showing `text`, as a host projects it.
fn row_of(text: &str) -> Vec<Seen> {
    let mut row: Vec<Seen> = text.chars().map(|c| Seen::of_cell(c, false)).collect();
    row.resize(80, Seen::Blank);
    row
}

/// The press of `←` with the cursor at `(0, cursor)` over row 0 showing `ab`.
fn press_left(p: &mut Predictor, cursor: u16, at: Instant) -> EditOutcome {
    let row = row_of("ab");
    let sample = KeySample {
        cursor: (0, cursor),
        coords: G,
        grid: (80, 24),
        row: Some(&row),
    };
    p.predict_edit(EditKey::Left, &sample, at)
}

/// The shipping predictor through the class ladder: a slow proven line `ab`
/// typed with its rows and echoed through `reconcile_seen` (so the edit lane may
/// place an arrow on it), `←` pressed, its echo, then a second `←`. Returns
/// whether each `←` was painted (its cursor in the overlay), and the predictor
/// with the instant. With `rule_off` the class proof is switched off.
fn class_ladder_run(rule_off: bool) -> (bool, bool, Predictor, Instant) {
    let mut p = Predictor::new(PredictMode::Adaptive);
    if rule_off {
        p.disable_rule_for_test(Rule::MotionClassProof);
    }
    let slow = Duration::from_millis(60);
    let mut at = Instant::now();
    for (col, ch) in [(0u16, 'a'), (1, 'b')] {
        let before: String = "ab".chars().take(usize::from(col)).collect();
        let row = row_of(&before);
        let sample = KeySample {
            cursor: (0, col),
            coords: G,
            grid: (80, 24),
            row: Some(&row),
        };
        assert!(p.predict_glyph(ch, &sample, at));
        at += slow;
        let after: String = "ab".chars().take(usize::from(col) + 1).collect();
        p.reconcile_seen(Some((0, col + 1)), G, false, at, |r, c| {
            let ch = (r == 0)
                .then(|| after.chars().nth(usize::from(c)))
                .flatten();
            ch.map_or(Seen::Blank, |ch| Seen::of_cell(ch, false))
        });
    }
    let shown = |p: &mut Predictor, at: Instant| {
        let _ = p.overlay(at);
        p.overlay_cursor().is_some()
    };
    let seen_ab = |r: u16, c: u16| match (r, c) {
        (0, 0) => Seen::Glyph('a'),
        (0, 1) => Seen::Glyph('b'),
        _ => Seen::Blank,
    };
    assert_eq!(press_left(&mut p, 2, at), EditOutcome::Armed);
    p.reconcile_seen(Some((0, 2)), G, false, at, seen_ab);
    let first = shown(&mut p, at);
    at += slow;
    p.reconcile_seen(Some((0, 1)), G, false, at, seen_ab);
    assert!(p.idle(), "the first ← echoed and retired");
    assert_eq!(press_left(&mut p, 1, at), EditOutcome::Armed);
    p.reconcile_seen(Some((0, 1)), G, false, at, seen_ab);
    let second = shown(&mut p, at);
    (first, second, p, at)
}

/// THE EDIT CLASS: a motion displays only once a motion of its epoch has
/// echoed. The model's `KeyEdit` before any `ConfirmEdit` is invisible, and so
/// is the shipping predictor's first `←`; its echo is `ConfirmEdit`; the second
/// `←` is visible on both sides; `note_line_submit` ends both proofs, `Submit`.
/// Tier-0 controls: the states a missing class proof and a class proof outliving
/// the glyph proof produce are rejected. Negative control, from the code: the
/// same drive with `Rule::MotionClassProof` off paints the first `←`, and that
/// pixel, projected onto the model's state at that step, is what
/// `EditVisibleNeedsClassProof` rejects; if the mutated code stopped painting it,
/// the control would prove nothing and fails instead.
#[test]
fn an_edit_displays_only_after_its_class_is_proven() {
    let model = aterm_models_core::predictive_echo_visibility_model();
    let mut st = model.init_state();
    assert!(model.fire("ConfirmSlow", &mut st));
    let (first, second, mut p, at) = class_ladder_run(false);
    assert!(model.fire("KeyEdit", &mut st));
    assert_eq!((st["visible"], st["class"]), (0, 1));
    assert!(
        !first,
        "the first ← is dark: no motion of this epoch has echoed"
    );
    let unproven = st.clone();
    assert!(model.fire("ConfirmEdit", &mut st));
    assert_eq!(st["edit_proven"], 1);
    assert!(model.fire("KeyEdit", &mut st));
    assert_eq!(st["visible"], 1);
    assert!(second, "the second ← shows");
    p.note_line_submit(at);
    assert!(model.fire("Submit", &mut st));
    assert_eq!((st["edit_proven"], st["confirmed"]), (0, 0));
    assert_eq!(
        press_left(&mut p, 1, at),
        EditOutcome::Declined,
        "nothing is placed on the line the terminator ended"
    );

    let mut leaked = model.init_state();
    for (var, value) in [
        ("visible", 1),
        ("class", 1),
        ("confirmed", 1),
        ("slow", 1),
        ("pending", 1),
        ("fresh", 0),
    ] {
        leaked.insert(var, value);
    }
    assert!(!model.check_invariant("EditVisibleNeedsClassProof", &leaked));
    let mut orphan = model.init_state();
    orphan.insert("edit_proven", 1);
    assert!(!model.check_invariant("ClassProofRidesOnGlyphProof", &orphan));

    let (mutant_first, ..) = class_ladder_run(true);
    assert!(mutant_first, "the mutated gate paints the first ←");
    let mut borrowed = unproven;
    borrowed.insert("visible", i64::from(mutant_first));
    assert!(
        !model.check_invariant("EditVisibleNeedsClassProof", &borrowed),
        "a motion shown before its class is proven must be rejected"
    );
}
