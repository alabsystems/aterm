// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Tier-1 binding between the derived predictive-visibility model and the genuine `aterm-predict`
//! state machine.

use std::time::{Duration, Instant};

use aterm_predict::{
    EditKey, EditOutcome, EditorContext, InputStart, KeySample, PredictMode, Predictor, Rule, Seen,
};

const G: u64 = 0;

fn echoed_a(row: u16, col: u16) -> Option<char> {
    ((row, col) == (0, 0)).then_some('a')
}

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

    let tracked_fast_prediction = fast.next_deadline().is_some();
    assert!(
        tracked_fast_prediction,
        "shipping Predictor tracks fast-link `b`"
    );
    assert_eq!(fast_model["confirmed"], 1);
    let mut always = Predictor::new(PredictMode::Always);
    assert!(always.predict_char('a', (0, 0), G, 80, now));
    always.reconcile(Some((0, 1)), G, false, fast_echo, echoed_a);
    assert!(always.predict_char('b', (0, 1), G, 80, fast_echo));
    assert_ne!(
        always.overlay(fast_echo).len(),
        fast.overlay(fast_echo).len(),
        "without the Adaptive regime gate, shipping Predictor paints fast-link `b`"
    );
    let mut unbenefited = fast_model.clone();
    unbenefited.insert("visible", 1);
    assert!(
        !model.check_invariant("FastAdaptiveNeverPaints", &unbenefited)
            && !model.check_invariant("VisibleNeedsProofAndBenefit", &unbenefited),
        "model invariants reject visible=1 while slow=0"
    );

    let expired = fast_echo + Duration::from_millis(251);
    assert!(model.fire("Expire", &mut fast_model));
    assert_eq!(fast_model["erased"], 0);
    assert_eq!((fast_model["owed"], fast_model["confirmed"]), (1, 0));
    assert!(fast.overlay(expired).is_empty());
    assert!(fast.idle());

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

    let mut inherited = model.init_state();
    inherited.insert("slow", 1);
    assert!(!model.check_invariant("FreshSessionHasNoInheritedRtt", &inherited));
}

#[test]
fn the_display_gate_closes_only_on_sustained_fast_evidence() {
    let model = aterm_models_core::predictive_echo_visibility_model();
    let mut st = model.init_state();
    let mut p = Predictor::new(PredictMode::Adaptive);
    let now = Instant::now();
    let slow = Duration::from_millis(60);
    let fast = Duration::from_millis(1);

    let mut at = echo_turn(&mut p, 'a', 0, now, slow);
    at = echo_turn(&mut p, 'b', 1, at, slow);
    assert!(model.fire("ConfirmSlow", &mut st));
    assert!(p.predict_char('c', (0, 2), G, 80, at));
    assert!(model.fire("Key", &mut st));
    assert_eq!(st["visible"], 1);
    assert_eq!(p.overlay(at).len(), 1, "control: the slow link paints");

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

#[test]
fn every_epoch_boundary_is_the_models_submit() {
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
        ("the rows moved under the cursor", |_, _| (G + 1, 2)),
        (
            "the program moved the cursor off the proven cell",
            |_, _| (G, 14),
        ),
        ("an arrow the predictor cannot place", |p, at| {
            let row = vec![Seen::Blank; 80];
            let sample = KeySample {
                cursor: (0, 2),
                coords: G,
                grid: (80, 24),
                row: Some(&row),
                ..KeySample::default()
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
        let action = if coords == G { "Submit" } else { "RowsMoved" };
        assert!(model.fire(action, &mut st), "{label}");
        assert_eq!(st["confirmed"], 0);
        assert!(p.predict_char('s', (0, col), coords, 80, at), "{label}");
        assert!(model.fire("Key", &mut st));
        assert_eq!(st["visible"], 0);
        assert!(
            p.overlay(at).is_empty(),
            "{label}: the next keystroke painted"
        );
    }

    let mut leaked = model.init_state();
    leaked.insert("slow", 1);
    leaked.insert("fresh", 0);
    leaked.insert("pending", 1);
    leaked.insert("visible", 1);
    assert!(!model.check_invariant("VisibleNeedsProofAndBenefit", &leaked));
}

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
    let (mut p, mut at, shipping_proven) = owed_echo_run(Duration::from_millis(10));
    assert!(model.fire("Key", &mut st));
    assert_eq!(st["visible"], 0);
    assert!(
        !shipping_proven,
        "the space echoes after the second 'n', not at (1, 3)"
    );

    let (_, _, credited) = owed_echo_run(Duration::from_millis(300));
    let mut borrowed = st.clone();
    borrowed.insert("confirmed", i64::from(credited));
    assert!(
        !model.check_invariant("ProofIsAttributable", &borrowed),
        "a proof borrowed from an owed echo must be rejected (credited: {credited})"
    );

    at += slow;
    let second_echo =
        |row: u16, col: u16| first_echo(row, col).or(((row, col) == (1, 3)).then_some('n'));
    p.reconcile(Some((1, 4)), G, false, at, second_echo);
    assert!(p.idle());
    assert!(model.fire("Submit", &mut st));

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

fn row_of(text: &str) -> Vec<Seen> {
    let mut row: Vec<Seen> = text.chars().map(|c| Seen::of_cell(c, false)).collect();
    row.resize(80, Seen::Blank);
    row
}

fn sample(row: &[Seen], cursor: u16) -> KeySample<'_> {
    KeySample {
        cursor: (0, cursor),
        coords: G,
        grid: (80, 24),
        row: Some(row),
        ..KeySample::default()
    }
}

fn press(p: &mut Predictor, key: EditKey, text: &str, cursor: u16, at: Instant) -> EditOutcome {
    let row = row_of(text);
    p.predict_edit(key, &sample(&row, cursor), at)
}

fn reconcile(p: &mut Predictor, text: &str, cursor: u16, at: Instant) {
    p.reconcile_seen(Some((0, cursor)), G, false, at, |r, c| {
        let ch = (r == 0).then(|| text.chars().nth(usize::from(c))).flatten();
        ch.map_or(Seen::Blank, |ch| Seen::of_cell(ch, false))
    });
}

/// The edit classes THE CLASS LADDER proves one at a time.
#[derive(Clone, Copy, Debug)]
enum EditClass {
    Motion,
    InsertShift,
    EraseShift,
}

impl EditClass {
    fn rule(self) -> Rule {
        match self {
            Self::Motion => Rule::MotionClassProof,
            Self::InsertShift => Rule::InsertShiftProof,
            Self::EraseShift => Rule::EraseShiftProof,
        }
    }
}

fn class_ladder_run(class: EditClass, rule_off: bool) -> (bool, bool, Predictor, Instant) {
    let mut p = Predictor::new(PredictMode::Adaptive);
    if rule_off {
        p.disable_rule_for_test(class.rule());
    }
    let slow = Duration::from_millis(60);
    let text = match class {
        EditClass::Motion => "ab",
        EditClass::InsertShift | EditClass::EraseShift => "abc",
    };
    let mut at = Instant::now();
    for (col, ch) in (0u16..).zip(text.chars()) {
        let before: String = text.chars().take(usize::from(col)).collect();
        let row = row_of(&before);
        assert!(p.predict_glyph(ch, &sample(&row, col), at));
        at += slow;
        let after: String = text.chars().take(usize::from(col) + 1).collect();
        reconcile(&mut p, &after, col + 1, at);
    }
    let shown = |p: &mut Predictor, at: Instant| {
        let _ = p.overlay(at);
        p.overlay_cursor().is_some()
    };
    let (first, second) = match class {
        EditClass::Motion => {
            assert_eq!(
                press(&mut p, EditKey::Left, "ab", 2, at),
                EditOutcome::Armed
            );
            reconcile(&mut p, "ab", 2, at);
            let first = shown(&mut p, at);
            at += slow;
            reconcile(&mut p, "ab", 1, at);
            assert!(p.idle(), "the first ← echoed and retired");
            assert_eq!(
                press(&mut p, EditKey::Left, "ab", 1, at),
                EditOutcome::Armed
            );
            reconcile(&mut p, "ab", 1, at);
            (first, shown(&mut p, at))
        }
        EditClass::InsertShift => {
            for cursor in [3, 2] {
                assert_eq!(
                    press(&mut p, EditKey::Left, "abc", cursor, at),
                    EditOutcome::Armed
                );
                at += slow;
                reconcile(&mut p, "abc", cursor - 1, at);
                assert!(p.idle(), "`←` echoed: the cursor on `b`");
            }
            let row = row_of("abc");
            assert!(p.predict_glyph('x', &sample(&row, 1), at));
            reconcile(&mut p, "abc", 1, at);
            let first = shown(&mut p, at);
            at += slow;
            reconcile(&mut p, "axbc", 2, at);
            assert!(p.idle(), "the first insert echoed, `bc` moved");
            let row = row_of("axbc");
            assert!(p.predict_glyph('y', &sample(&row, 2), at));
            reconcile(&mut p, "axbc", 2, at);
            (first, shown(&mut p, at))
        }
        EditClass::EraseShift => {
            assert_eq!(
                press(&mut p, EditKey::Left, "abc", 3, at),
                EditOutcome::Armed
            );
            at += slow;
            reconcile(&mut p, "abc", 2, at);
            assert!(p.idle(), "`←` echoed: the cursor on `c`");
            assert_eq!(
                press(&mut p, EditKey::Backspace, "abc", 2, at),
                EditOutcome::Armed
            );
            reconcile(&mut p, "abc", 2, at);
            let first = shown(&mut p, at);
            at += slow;
            reconcile(&mut p, "ac", 1, at);
            assert!(p.idle(), "the first erase echoed, `c` moved");
            assert_eq!(
                press(&mut p, EditKey::Backspace, "ac", 1, at),
                EditOutcome::Armed
            );
            reconcile(&mut p, "ac", 1, at);
            (first, shown(&mut p, at))
        }
    };
    (first, second, p, at)
}

#[test]
fn an_edit_displays_only_after_its_class_is_proven() {
    let model = aterm_models_core::predictive_echo_visibility_model();
    for class in [
        EditClass::Motion,
        EditClass::InsertShift,
        EditClass::EraseShift,
    ] {
        let mut st = model.init_state();
        assert!(model.fire("ConfirmSlow", &mut st));
        let (first, second, mut p, at) = class_ladder_run(class, false);
        assert!(model.fire("KeyEdit", &mut st));
        assert_eq!((st["visible"], st["class"]), (0, 1));
        assert!(
            !first,
            "{class:?}: the first op is dark, no op of its class has echoed"
        );
        let unproven = st.clone();
        assert!(model.fire("ConfirmEdit", &mut st));
        assert_eq!(st["edit_proven"], 1);
        assert!(model.fire("KeyEdit", &mut st));
        assert_eq!(st["visible"], 1);
        assert!(second, "{class:?}: the second op shows");
        p.note_line_submit(at);
        assert!(model.fire("Submit", &mut st));
        assert_eq!((st["edit_proven"], st["confirmed"]), (0, 0));
        assert_eq!(
            press(&mut p, EditKey::Left, "abc", 1, at),
            EditOutcome::Declined,
            "{class:?}: nothing is placed on the line the terminator ended"
        );

        let (mutant_first, ..) = class_ladder_run(class, true);
        assert!(
            mutant_first,
            "{class:?}: the mutated gate paints the first op"
        );
        let mut borrowed = unproven;
        borrowed.insert("visible", i64::from(mutant_first));
        assert!(
            !model.check_invariant("EditVisibleNeedsClassProof", &borrowed),
            "{class:?}: an op shown before its class is proven must be rejected"
        );
    }

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
}

fn signed(row: &[Seen], cursor: u16, start: Option<InputStart>) -> KeySample<'_> {
    KeySample {
        input_start: start,
        context: EditorContext::new(1, Some(&[7; 32]), false, false),
        ..sample(row, cursor)
    }
}

const B: InputStart = InputStart {
    cell: (0, 0),
    prompt: 1,
};

/// How a line-edge run starts its line.
#[derive(Clone, Copy)]
struct Start {
    hands_b: bool,
    first_key: bool,
    scrolled: bool,
}

const WITNESSED: Start = Start {
    hands_b: true,
    first_key: true,
    scrolled: false,
};

fn typed_ab(p: &mut Predictor, start: Start) -> Instant {
    let slow = Duration::from_millis(60);
    let mut at = Instant::now();
    if start.scrolled {
        p.reconcile_seen(Some((0, 0)), G + 1, false, at, |_, _| Seen::Blank);
    }
    if start.first_key {
        p.note_line_submit_at(Some(B.prompt - 1), at);
        at += p.glitch_window_for_test() + Duration::from_millis(1);
    }
    if start.scrolled {
        reconcile(p, "", 0, at);
    }
    for (col, ch) in (0u16..).zip("ab".chars()) {
        let before: String = "ab".chars().take(usize::from(col)).collect();
        let row = row_of(&before);
        let b = start.hands_b.then_some(B);
        assert!(p.predict_glyph(ch, &signed(&row, col, b), at));
        at += slow;
        let after: String = "ab".chars().take(usize::from(col) + 1).collect();
        reconcile(p, &after, col + 1, at);
    }
    at
}

fn line_edge_run(off: Option<Rule>, start: Start) -> ([bool; 3], Predictor, Instant) {
    let mut p = Predictor::new(PredictMode::Adaptive);
    if let Some(rule) = off {
        p.disable_rule_for_test(rule);
    }
    let slow = Duration::from_millis(60);
    let mut at = typed_ab(&mut p, start);
    let row = row_of("ab");
    let mut shown = [false; 3];
    for (round, painted) in shown.iter_mut().enumerate() {
        assert_eq!(
            p.predict_edit(EditKey::Home, &signed(&row, 2, Some(B)), at),
            EditOutcome::Armed,
            "Home, round {round}"
        );
        reconcile(&mut p, "ab", 2, at);
        let _ = p.overlay(at);
        *painted = p.overlay_cursor().is_some();
        at += slow;
        reconcile(&mut p, "ab", 0, at);
        assert!(p.idle(), "Home's echo retires it, round {round}");
        if round < 2 {
            for cursor in [0, 1] {
                assert_eq!(
                    press(&mut p, EditKey::Right, "ab", cursor, at),
                    EditOutcome::Armed
                );
                at += slow;
                reconcile(&mut p, "ab", cursor + 1, at);
                assert!(p.idle(), "`→` echoed");
            }
        }
    }
    (shown, p, at)
}

#[test]
fn a_line_edge_key_displays_only_when_learned_at_a_witnessed_prompt() {
    let model = aterm_models_core::predictive_echo_visibility_model();
    let mut st = model.init_state();
    assert!(model.fire("SubmitAtPrompt", &mut st));
    assert!(model.fire("OwedExpire", &mut st));
    assert!(model.fire("Key", &mut st));
    assert_eq!(st["first"], 1, "the prompt's first key");
    assert!(model.fire("Echo", &mut st));
    assert!(model.fire("ConfirmSlow", &mut st));
    assert!(model.fire("Witness", &mut st));
    let (shown, mut p, at) = line_edge_run(None, WITNESSED);
    assert!(model.fire("KeyLineEdge", &mut st));
    assert_eq!((st["visible"], st["class"], st["learned"]), (0, 2, 0));
    assert!(!shown[0], "the first Home is a probe: dark");
    let unlearned = st.clone();
    assert!(model.fire("Echo", &mut st));
    assert!(model.fire("KeyLineEdge", &mut st));
    assert_eq!(st["visible"], 0);
    assert!(!shown[1], "one echo is not two: still a probe");
    assert!(model.fire("ConfirmLineEdge", &mut st));
    assert_eq!(st["learned"], 1);
    assert!(model.fire("KeyLineEdge", &mut st));
    assert_eq!(st["visible"], 1);
    assert!(shown[2], "learned: the third Home shows");
    p.note_line_submit(at);
    assert!(model.fire("Echo", &mut st));
    assert!(model.fire("Submit", &mut st));
    assert_eq!((st["witness"], st["confirmed"], st["gate"]), (0, 0, 0));
    assert!(
        !model.fire("KeyLineEdge", &mut st.clone()),
        "no line-edge key without a witness"
    );
    let row = row_of("ab");
    assert_eq!(
        p.predict_edit(EditKey::Home, &signed(&row, 0, Some(B)), at),
        EditOutcome::Declined,
        "nothing is placed on the line the terminator ended"
    );
    let mut later = model.init_state();
    assert!(model.fire("Key", &mut later));
    assert!(model.fire("Echo", &mut later));
    assert!(model.fire("ConfirmSlow", &mut later));
    assert!(!model.fire("Witness", &mut later.clone()), "no first key");

    let (mutant, ..) = line_edge_run(Some(Rule::LearnedBindings), WITNESSED);
    assert!(mutant[0], "the mutated gate paints the first Home");
    let mut borrowed = unlearned;
    borrowed.insert("visible", i64::from(mutant[0]));
    assert!(
        !model.check_invariant("LineEdgeNeedsWitnessAndBinding", &borrowed),
        "a line-edge key shown before its binding is learned must be rejected"
    );

    let (mutant, ..) = line_edge_run(
        Some(Rule::WitnessFreshPrompt),
        Start {
            hands_b: false,
            ..WITNESSED
        },
    );
    assert!(mutant[2], "the mutated witness paints a learned Home");
    let mut unwitnessed = model.init_state();
    assert!(model.fire("ConfirmSlow", &mut unwitnessed));
    for (var, value) in [
        ("learned", 1),
        ("pending", 1),
        ("class", 2),
        ("visible", i64::from(mutant[2])),
    ] {
        unwitnessed.insert(var, value);
    }
    assert!(
        !model.check_invariant("LineEdgeNeedsWitnessAndBinding", &unwitnessed),
        "a line-edge key shown on a line no B witnessed must be rejected"
    );

    let (mutant, ..) = line_edge_run(
        Some(Rule::WitnessFirstKey),
        Start {
            first_key: false,
            ..WITNESSED
        },
    );
    assert!(
        mutant[2],
        "the mutated first key paints a learned Home on a later key's line"
    );
    let mut not_first = later;
    for (var, value) in [
        ("witness", 1),
        ("witness_first", 0),
        ("learned", 1),
        ("pending", 1),
        ("class", 2),
        ("visible", i64::from(mutant[2])),
    ] {
        not_first.insert(var, value);
    }
    assert!(
        !model.check_invariant("WitnessIsTheFirstKey", &not_first),
        "a line witnessed by a key that was not its prompt's first must be rejected"
    );

    let mut leaked = model.init_state();
    for (var, value) in [
        ("visible", 1),
        ("class", 2),
        ("confirmed", 1),
        ("slow", 1),
        ("pending", 1),
        ("fresh", 0),
        ("witness", 1),
        ("witness_first", 1),
    ] {
        leaked.insert(var, value);
    }
    assert!(!model.check_invariant("LineEdgeNeedsWitnessAndBinding", &leaked));
    let mut kept = model.init_state();
    kept.insert("learned", 1);
    assert!(!model.check_invariant("FreshContextHasNoBinding", &kept));
}

#[test]
fn a_scroll_keeps_the_first_key_s_gate() {
    let model = aterm_models_core::predictive_echo_visibility_model();
    let mut st = model.init_state();
    assert!(model.fire("SubmitAtPrompt", &mut st));
    assert!(model.fire("RowsMoved", &mut st));
    assert_eq!((st["gate"], st["confirmed"], st["owed"]), (1, 0, 1));
    assert!(model.fire("OwedExpire", &mut st));
    assert!(model.fire("Key", &mut st));
    assert_eq!(st["first"], 1, "still the prompt's first key");
    assert!(model.fire("Echo", &mut st));
    assert!(model.fire("ConfirmSlow", &mut st));
    assert!(model.fire("Witness", &mut st));
    assert!(model.check_invariant("WitnessIsTheFirstKey", &st));
    let scrolled = Start {
        scrolled: true,
        ..WITNESSED
    };
    let (shown, ..) = line_edge_run(None, scrolled);
    assert!(model.fire("KeyLineEdge", &mut st));
    assert!(model.fire("Echo", &mut st));
    assert!(model.fire("KeyLineEdge", &mut st));
    assert!(model.fire("ConfirmLineEdge", &mut st));
    assert!(model.fire("KeyLineEdge", &mut st));
    assert_eq!(st["visible"], 1);
    assert_eq!(shown, [false, false, true], "the third, learned Home shows");

    let mut reset = model.init_state();
    assert!(model.fire("SubmitAtPrompt", &mut reset));
    assert!(model.fire("Submit", &mut reset));
    assert!(model.fire("OwedExpire", &mut reset));
    assert!(model.fire("Key", &mut reset));
    assert!(model.fire("Echo", &mut reset));
    assert!(model.fire("ConfirmSlow", &mut reset));
    assert_eq!(reset["first"], 0);
    assert!(!model.fire("Witness", &mut reset.clone()), "no first key");

    for (rule_on, want) in [(true, EditOutcome::Armed), (false, EditOutcome::Declined)] {
        let mut p = Predictor::new(PredictMode::Adaptive);
        if !rule_on {
            p.disable_rule_for_test(Rule::ScrollKeepsFirstKey);
        }
        let at = typed_ab(&mut p, scrolled);
        let outcome = p.predict_edit(EditKey::Home, &signed(&row_of("ab"), 2, Some(B)), at);
        assert_eq!(outcome, want, "rule on {rule_on}");
    }
}
