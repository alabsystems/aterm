// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! The `App::input` overlay gate preserves Kitty press/release pairing
//! across overlay open/close. The observable is the seam's reply on the
//! stub session's invalid (-1) sink with Kitty REPORT_EVENT_TYPES
//! (`CSI > 2 u`) negotiated: an encoded event reports `WriteFailed`, while
//! a swallowed one returns `Ok` without touching the sink. `Source` is
//! audit-only, so `Source::Human` exercises the same gate a controller
//! event hits.
use crate::input::{InputEvent, InputOutcome, Source};
use crate::{App, WindowId, term_lock};
use aterm_types::keyboard::{Key, KeyEventType, Modifiers, NamedKey};

fn named(key: NamedKey, event_type: KeyEventType) -> InputEvent {
    InputEvent::Key {
        key: Key::Named(key),
        mods: Modifiers::empty(),
        base_layout: None,
        layout_text: None,
        event_type,
    }
}

fn pairing_step(
    model: &aterm_spec::derive::Model,
    state: &mut aterm_spec::interp::State,
    action: &'static str,
) {
    assert!(model.fire(action, state), "{action}: {state:?}");
    for invariant in &model.invariants {
        assert!(
            model.check_invariant(invariant.name, state),
            "shipping overlay gate violates {} after {action}: {state:?}",
            invariant.name,
        );
    }
}
/// An app that negotiated Kitty event-type reporting. Functional-key
/// releases encode bytes (a swallow is observable as `Ok` vs
/// `WriteFailed`); plain text releases stay silent unless REPORT_ALL_KEYS is
/// active.
fn app_with_event_types() -> App {
    let app = App::headless_for_test();
    let term = app
        .front_terminal(WindowId(0))
        .expect("front terminal")
        .term
        .clone();
    term_lock(&term).process(b"\x1b[>2u");
    app
}

/// A release whose press predates the overlay (delivered to the PTY, then
/// the overlay opened mid-hold via menu click / aterm-ctl) falls through the
/// gate to the seam encoder; swallowing it would leave the app an orphan
/// press, and every overlay handler ignores releases anyway.
#[test]
fn release_of_pre_overlay_press_falls_through_the_gate() {
    let mut app = app_with_event_types();
    let wid = WindowId(0);
    let model = aterm_spec::derive::input_release_pairing_model();
    let mut state = model.init_state();
    // The press reached the seam (WriteFailed on the -1 sink proves it encoded).
    assert_eq!(
        app.input(
            wid,
            named(NamedKey::ArrowUp, KeyEventType::Press),
            Source::Human,
        ),
        InputOutcome::WriteFailed,
        "no overlay: the press reaches the PTY"
    );
    pairing_step(&model, &mut state, "ForwardPress");
    app.palette_enter();
    pairing_step(&model, &mut state, "OpenOverlay");
    assert!(app.windows.get(&wid).unwrap().overlay_open());

    // Tier-1 negative control: swallowing this untracked release under the
    // now-open overlay strands the press the Kitty app already observed.
    let buggy = aterm_spec::interp::with_buggy(&model, 1);
    let mut swallowed = state.clone();
    assert!(buggy.fire("ReleaseForwardedPress", &mut swallowed));
    assert!(!buggy.check_invariant("UntrackedReleaseNeverSwallowed", &swallowed,));
    // The in-flight release must still reach the seam (encode + fail), not be
    // swallowed to Ok by the gate.
    assert_eq!(
        app.input(
            wid,
            named(NamedKey::ArrowUp, KeyEventType::Release),
            Source::Human,
        ),
        InputOutcome::WriteFailed,
        "the release pairs with the pre-overlay press: it falls through to the encoder"
    );
    pairing_step(&model, &mut state, "ReleaseForwardedPress");
    assert!(
        app.windows.get(&wid).unwrap().overlay_open(),
        "the fall-through never closes/steals the overlay"
    );
}

/// A controller press consumed by the overlay gate is recorded, and its
/// release arriving after the overlay closed is swallowed exactly once
/// (otherwise it encodes an orphan Kitty release report).
#[test]
fn press_under_overlay_release_swallowed_after_close() {
    let mut app = app_with_event_types();
    let wid = WindowId(0);
    let model = aterm_spec::derive::input_release_pairing_model();
    let mut state = model.init_state();
    app.palette_enter();
    pairing_step(&model, &mut state, "OpenOverlay");
    assert_eq!(
        app.input(
            wid,
            named(NamedKey::ArrowRight, KeyEventType::Press),
            Source::Human,
        ),
        InputOutcome::Ok,
        "the overlay consumes the press (routed, not encoded)"
    );
    pairing_step(&model, &mut state, "ConsumeOverlayPress");
    assert!(
        app.windows
            .get(&wid)
            .unwrap()
            .overlay_consumed_keys
            .contains(&Key::Named(NamedKey::ArrowRight)),
        "the consumed press is recorded at the seam"
    );
    app.palette_exit();
    pairing_step(&model, &mut state, "CloseOverlay");
    assert!(!app.windows.get(&wid).unwrap().overlay_open());
    assert_eq!(
        app.input(
            wid,
            named(NamedKey::ArrowRight, KeyEventType::Release),
            Source::Human,
        ),
        InputOutcome::Ok,
        "the release is swallowed even though the overlay already closed"
    );
    pairing_step(&model, &mut state, "ReleaseConsumedPress");
    assert!(
        app.windows
            .get(&wid)
            .unwrap()
            .overlay_consumed_keys
            .is_empty(),
        "swallow-once: the entry is removed"
    );
    // A later unrelated release of the same key encodes as today (reaches the
    // seam and fails on the stub sink) — no stale double-swallow.
    assert_eq!(
        app.input(
            wid,
            named(NamedKey::ArrowRight, KeyEventType::Release),
            Source::Human,
        ),
        InputOutcome::WriteFailed
    );
}

/// The same pairing while the overlay stays open: press consumed and
/// recorded, release swallowed and the entry removed. A repeat under the
/// overlay does not record: a repeat of a pre-overlay press would otherwise
/// swallow a release the app is owed.
#[test]
fn overlay_repeat_does_not_record_and_open_overlay_release_pairs() {
    let mut app = app_with_event_types();
    let wid = WindowId(0);
    let model = aterm_spec::derive::input_release_pairing_model();
    let mut pre_overlay_hold = model.init_state();
    pairing_step(&model, &mut pre_overlay_hold, "ForwardPress");
    app.palette_enter();
    pairing_step(&model, &mut pre_overlay_hold, "OpenOverlay");
    // Repeat routed to the overlay: swallowed but not recorded.
    assert_eq!(
        app.input(
            wid,
            named(NamedKey::ArrowUp, KeyEventType::Repeat),
            Source::Human,
        ),
        InputOutcome::Ok
    );
    pairing_step(
        &model,
        &mut pre_overlay_hold,
        "GateConsumesRepeatOfForwardedPress",
    );
    assert!(
        app.windows
            .get(&wid)
            .unwrap()
            .overlay_consumed_keys
            .is_empty(),
        "a repeat never records a consumed press at the gate"
    );
    // Its release (press predated the overlay) falls through to the encoder.
    assert_eq!(
        app.input(
            wid,
            named(NamedKey::ArrowUp, KeyEventType::Release),
            Source::Human,
        ),
        InputOutcome::WriteFailed,
        "the repeat did not poison the release disposition"
    );
    pairing_step(&model, &mut pre_overlay_hold, "ReleaseForwardedPress");
    // Press+release both under the open overlay: consumed, then swallowed.
    let mut under_overlay = model.init_state();
    pairing_step(&model, &mut under_overlay, "OpenOverlay");
    assert_eq!(
        app.input(
            wid,
            named(NamedKey::ArrowDown, KeyEventType::Press),
            Source::Human,
        ),
        InputOutcome::Ok
    );
    pairing_step(&model, &mut under_overlay, "ConsumeOverlayPress");
    assert_eq!(
        app.input(
            wid,
            named(NamedKey::ArrowDown, KeyEventType::Release),
            Source::Human,
        ),
        InputOutcome::Ok,
        "an overlay-consumed press has its release swallowed while the overlay is open"
    );
    pairing_step(&model, &mut under_overlay, "ReleaseConsumedPress");
    assert!(
        app.windows
            .get(&wid)
            .unwrap()
            .overlay_consumed_keys
            .is_empty(),
        "the release removed its entry (leak-free)"
    );
}
