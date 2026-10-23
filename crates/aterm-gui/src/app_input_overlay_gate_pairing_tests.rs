// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Tests that the `App::input` overlay gate preserves Kitty press/release pairing.
use crate::input::{InputEvent, InputOutcome, Source};
use crate::{App, WindowId, term_lock};
use aterm_types::keyboard::{Key, KeyEventType, Modifiers, NamedKey};

use super::paste_order;

fn named(key: NamedKey, event_type: KeyEventType) -> InputEvent {
    InputEvent::Key {
        key: Key::Named(key),
        mods: Modifiers::empty(),
        base_layout: None,
        layout_text: None,
        event_type,
    }
}

/// What the seam did with one event.
#[derive(Debug, PartialEq, Eq)]
enum Seam {
    /// It encoded the event for the PTY: on the inline route the write to the -1
    /// sink fails at once (`WriteFailed`); on the ConPTY route (every key on
    /// Windows) the event is handed to the session's ordered writer (FIFO
    /// admission, `Ok`) and the write fails later, on the writer thread.
    Encoded,
    /// The gate swallowed it: `Ok`, and nothing written or handed over.
    Swallowed,
}

fn dispatch(app: &mut App, wid: WindowId, ev: InputEvent) -> Seam {
    let before = paste_order::submitted_on_this_thread_for_test();
    let outcome = app.input(wid, ev, Source::Human);
    let handed = paste_order::submitted_on_this_thread_for_test() - before;
    match (outcome, handed) {
        (InputOutcome::WriteFailed, 0) | (InputOutcome::Ok, 1) => Seam::Encoded,
        (InputOutcome::Ok, 0) => Seam::Swallowed,
        other => panic!("neither an encode nor a swallow: {other:?}"),
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

#[test]
fn release_of_pre_overlay_press_falls_through_the_gate() {
    for &conpty in paste_order::routes_for_test() {
        let _route = conpty.then(paste_order::force_conpty_route_for_test);
        let mut app = app_with_event_types();
        let wid = WindowId(0);
        let model = aterm_models_misc::input_release_pairing_model();
        let mut state = model.init_state();
        assert_eq!(
            dispatch(&mut app, wid, named(NamedKey::ArrowUp, KeyEventType::Press)),
            Seam::Encoded,
            "no overlay: the press reaches the PTY (conpty={conpty})"
        );
        pairing_step(&model, &mut state, "ForwardPress");
        app.palette_enter();
        pairing_step(&model, &mut state, "OpenOverlay");
        assert!(app.windows.get(&wid).unwrap().overlay_open());

        let buggy = aterm_spec::interp::with_buggy(&model, 1);
        let mut swallowed = state.clone();
        assert!(buggy.fire("ReleaseForwardedPress", &mut swallowed));
        assert!(!buggy.check_invariant("UntrackedReleaseNeverSwallowed", &swallowed,));
        assert_eq!(
            dispatch(
                &mut app,
                wid,
                named(NamedKey::ArrowUp, KeyEventType::Release)
            ),
            Seam::Encoded,
            "the release pairs with the pre-overlay press: it falls through to the \
             encoder (conpty={conpty})"
        );
        pairing_step(&model, &mut state, "ReleaseForwardedPress");
        assert!(
            app.windows.get(&wid).unwrap().overlay_open(),
            "the fall-through never closes/steals the overlay"
        );
    }
}

#[test]
fn press_under_overlay_release_swallowed_after_close() {
    for &conpty in paste_order::routes_for_test() {
        let _route = conpty.then(paste_order::force_conpty_route_for_test);
        let mut app = app_with_event_types();
        let wid = WindowId(0);
        let model = aterm_models_misc::input_release_pairing_model();
        let mut state = model.init_state();
        app.palette_enter();
        pairing_step(&model, &mut state, "OpenOverlay");
        assert_eq!(
            dispatch(
                &mut app,
                wid,
                named(NamedKey::ArrowRight, KeyEventType::Press)
            ),
            Seam::Swallowed,
            "the overlay consumes the press (routed, not encoded; conpty={conpty})"
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
            dispatch(
                &mut app,
                wid,
                named(NamedKey::ArrowRight, KeyEventType::Release)
            ),
            Seam::Swallowed,
            "the release is swallowed even though the overlay already closed \
             (conpty={conpty})"
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
        assert_eq!(
            dispatch(
                &mut app,
                wid,
                named(NamedKey::ArrowRight, KeyEventType::Release)
            ),
            Seam::Encoded,
            "conpty={conpty}"
        );
    }
}

#[test]
fn overlay_repeat_does_not_record_and_open_overlay_release_pairs() {
    for &conpty in paste_order::routes_for_test() {
        let _route = conpty.then(paste_order::force_conpty_route_for_test);
        let mut app = app_with_event_types();
        let wid = WindowId(0);
        let model = aterm_models_misc::input_release_pairing_model();
        let mut pre_overlay_hold = model.init_state();
        pairing_step(&model, &mut pre_overlay_hold, "ForwardPress");
        app.palette_enter();
        pairing_step(&model, &mut pre_overlay_hold, "OpenOverlay");
        assert_eq!(
            dispatch(
                &mut app,
                wid,
                named(NamedKey::ArrowUp, KeyEventType::Repeat)
            ),
            Seam::Swallowed,
            "conpty={conpty}"
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
        assert_eq!(
            dispatch(
                &mut app,
                wid,
                named(NamedKey::ArrowUp, KeyEventType::Release)
            ),
            Seam::Encoded,
            "the repeat did not poison the release disposition (conpty={conpty})"
        );
        pairing_step(&model, &mut pre_overlay_hold, "ReleaseForwardedPress");
        let mut under_overlay = model.init_state();
        pairing_step(&model, &mut under_overlay, "OpenOverlay");
        assert_eq!(
            dispatch(
                &mut app,
                wid,
                named(NamedKey::ArrowDown, KeyEventType::Press)
            ),
            Seam::Swallowed,
            "conpty={conpty}"
        );
        pairing_step(&model, &mut under_overlay, "ConsumeOverlayPress");
        assert_eq!(
            dispatch(
                &mut app,
                wid,
                named(NamedKey::ArrowDown, KeyEventType::Release)
            ),
            Seam::Swallowed,
            "an overlay-consumed press has its release swallowed while the overlay is \
             open (conpty={conpty})"
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
}
