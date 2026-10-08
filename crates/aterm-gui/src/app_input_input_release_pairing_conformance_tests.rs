// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use std::collections::BTreeMap;

use aterm_spec::derive::input_release_pairing_model;
use aterm_types::keyboard::{Key as TKey, Modifiers, NamedKey};
use winit::keyboard::{KeyCode, PhysicalKey};

use super::{PhysicalReleaseTrace, clear_physical_release_trace, take_physical_release_trace};
use crate::input::InputEvent;
use crate::{App, PhysicalPressOwner, WindowId, stub_session, term_lock};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Disposition {
    None,
    Consumed,
    Forwarded,
    Literal,
    Local,
}

/// Ghost fields that remain observable at the protocol boundary after the
/// real owner-map entry has been removed. All live-hold fields are projected
/// directly from `App::physical_press_owners`; only completed-episode history
/// (which the shipping map intentionally forgets) is retained here.
#[derive(Clone, Copy, Debug)]
pub(crate) struct ProjectionWitness {
    phase: i64,
    disposition: Disposition,
    press_window: Option<WindowId>,
    repeat_routed: Option<WindowId>,
    release_arrival: Option<WindowId>,
    release_routed: Option<WindowId>,
    repeat_observed: bool,
    repeat_emitted: bool,
    release_emitted: bool,
}

impl ProjectionWitness {
    fn initial() -> Self {
        Self {
            phase: 0,
            disposition: Disposition::None,
            press_window: None,
            repeat_routed: None,
            release_arrival: None,
            release_routed: None,
            repeat_observed: false,
            repeat_emitted: false,
            release_emitted: false,
        }
    }

    fn held(disposition: Disposition, press_window: WindowId) -> Self {
        Self {
            phase: 1,
            disposition,
            press_window: Some(press_window),
            repeat_routed: None,
            release_arrival: None,
            release_routed: None,
            repeat_observed: false,
            repeat_emitted: false,
            release_emitted: false,
        }
    }

    fn repeated(mut self, routed: WindowId) -> Self {
        self.repeat_routed = Some(routed);
        self.repeat_observed = true;
        self.repeat_emitted = true;
        self
    }

    fn silent_repeat(mut self) -> Self {
        self.repeat_observed = true;
        self
    }

    fn untracked_repeat(mut self) -> Self {
        self.repeat_observed = true;
        self
    }

    fn released(mut self, arrival: WindowId, routed: Option<WindowId>, emitted: bool) -> Self {
        self.phase = 2;
        self.release_arrival = Some(arrival);
        self.release_routed = routed;
        self.release_emitted = emitted;
        self
    }
}

fn model_window(actual: WindowId, window_a: WindowId, window_b: WindowId) -> i64 {
    if actual == window_a {
        1
    } else if actual == window_b {
        2
    } else {
        panic!("window {actual:?} is outside the two-window projection")
    }
}

fn focused_window(app: &App, window_a: WindowId, window_b: WindowId) -> i64 {
    let a = app.windows[&window_a].focused;
    let b = app.windows[&window_b].focused;
    match (a, b) {
        (true, false) => 1,
        (false, true) => 2,
        other => panic!("two-window conformance requires exactly one focus: {other:?}"),
    }
}

/// Structural projection named by all ownership `#[refines]` anchors. Owner kind,
/// original press window, and outstanding-forwarded authority come from the
/// real process-wide map; focus comes from the two real `WindowState`s.
pub(crate) fn project(
    app: &App,
    physical: PhysicalKey,
    window_a: WindowId,
    window_b: WindowId,
    witness: ProjectionWitness,
) -> aterm_spec::interp::State {
    let (tracker, outstanding) = match app.physical_press_owners.get(&physical) {
        Some(PhysicalPressOwner::Consumed { window }) => {
            assert_eq!(witness.phase, 1, "consumed owner exists only while held");
            assert_eq!(witness.disposition, Disposition::Consumed);
            assert_eq!(Some(*window), witness.press_window);
            (1, 0)
        }
        Some(PhysicalPressOwner::Forwarded { window, .. }) => {
            assert_eq!(witness.phase, 1, "forwarded owner exists only while held");
            assert_eq!(witness.disposition, Disposition::Forwarded);
            assert_eq!(Some(*window), witness.press_window);
            (0, 1)
        }
        Some(PhysicalPressOwner::Literal { window, .. }) => {
            assert_eq!(witness.phase, 1, "literal owner exists only while held");
            assert_eq!(witness.disposition, Disposition::Literal);
            assert_eq!(Some(*window), witness.press_window);
            (3, 0)
        }
        Some(PhysicalPressOwner::Local { window, .. }) => {
            assert_eq!(witness.phase, 1, "local owner exists only while held");
            assert_eq!(witness.disposition, Disposition::Local);
            assert_eq!(Some(*window), witness.press_window);
            (4, 0)
        }
        None => {
            assert_ne!(witness.phase, 1, "held projection lost its real owner");
            (0, 0)
        }
    };
    let press_consumed = i64::from(witness.disposition == Disposition::Consumed);
    let press_forwarded = i64::from(witness.disposition == Disposition::Forwarded);
    let press_literal = i64::from(witness.disposition == Disposition::Literal);
    let press_local = i64::from(witness.disposition == Disposition::Local);
    let map_optional = |window: Option<WindowId>| {
        window.map_or(0, |window| model_window(window, window_a, window_b))
    };
    [
        ("phase", witness.phase),
        ("tracker", tracker),
        ("overlay_open", 0),
        ("press_consumed", press_consumed),
        ("press_forwarded", press_forwarded),
        ("press_literal", press_literal),
        ("press_local", press_local),
        ("pty_press_outstanding", outstanding),
        ("repeat_observed", i64::from(witness.repeat_observed)),
        ("repeat_emitted", i64::from(witness.repeat_emitted)),
        ("release_emitted", i64::from(witness.release_emitted)),
        ("untracked_release_swallowed", 0),
        ("orphan_csi_u", 0),
        ("focused_window", focused_window(app, window_a, window_b)),
        ("press_window", map_optional(witness.press_window)),
        ("repeat_routed_window", map_optional(witness.repeat_routed)),
        (
            "release_arrival_window",
            map_optional(witness.release_arrival),
        ),
        (
            "release_routed_window",
            map_optional(witness.release_routed),
        ),
    ]
    .into_iter()
    .collect::<BTreeMap<_, _>>()
}

fn validate_transition(
    action: &'static str,
    prev: &aterm_spec::interp::State,
    next: &aterm_spec::interp::State,
) -> (bool, String) {
    aterm_spec::verify::validate_transition_tiered(
        &input_release_pairing_model(),
        &[],
        prev,
        next,
        Some(action),
        "App physical input release pairing conformance",
    )
}

fn require_transition(
    action: &'static str,
    prev: &aterm_spec::interp::State,
    next: &aterm_spec::interp::State,
) {
    let (ok, out) = validate_transition(action, prev, next);
    assert!(
        ok,
        "real {action} transition must conform:\nprev={prev:?}\nnext={next:?}\n{out}"
    );
}

fn set_focus(app: &mut App, window_a: WindowId, window_b: WindowId, focus_a: bool) {
    app.on_focus(window_a, focus_a);
    app.on_focus(window_b, !focus_a);
}

fn two_window_app() -> (App, WindowId, WindowId, u64, u64) {
    let mut app = App::headless_for_test();
    let window_a = WindowId(0);
    let session_a = app
        .front_terminal(window_a)
        .expect("window A terminal")
        .session;
    let session_b = app.next_session_id;
    let window_b = app.insert_logical_window(stub_session(session_b), 24, 80);
    assert_ne!(session_a, session_b, "windows must own distinct sessions");
    set_focus(&mut app, window_a, window_b, true);
    (app, window_a, window_b, session_a, session_b)
}

pub(crate) fn run_conformance() {
    let model = input_release_pairing_model();

    // A repeat arriving without any observed press epoch is fail-closed.
    let (mut untracked_app, untracked_a, untracked_b, _, _) = two_window_app();
    let untracked_physical = PhysicalKey::Code(KeyCode::KeyZ);
    let untracked_initial = project(
        &untracked_app,
        untracked_physical,
        untracked_a,
        untracked_b,
        ProjectionWitness::initial(),
    );
    clear_physical_release_trace();
    untracked_app.route_physical_repeat(untracked_b, untracked_physical);
    assert_eq!(
        take_physical_release_trace(),
        None,
        "untracked repeat is byte-silent"
    );
    let untracked_silent = project(
        &untracked_app,
        untracked_physical,
        untracked_a,
        untracked_b,
        ProjectionWitness::initial().untracked_repeat(),
    );
    require_transition(
        "SwallowUntrackedRepeat",
        &untracked_initial,
        &untracked_silent,
    );

    // Consumed A press -> focus B -> release delivered through B. The real
    // release branch removes A's owner/mirror and returns before seam egress.
    let (mut app, window_a, window_b, _, _) = two_window_app();
    let physical = PhysicalKey::Code(KeyCode::KeyA);
    let initial_witness = ProjectionWitness::initial();
    let initial = project(&app, physical, window_a, window_b, initial_witness);
    assert_eq!(
        initial,
        model.init_state(),
        "real projection matches model Init"
    );

    app.note_consumed_press_key(window_a, physical, false);
    let consumed_witness = ProjectionWitness::held(Disposition::Consumed, window_a);
    let consumed = project(&app, physical, window_a, window_b, consumed_witness);
    require_transition("ConsumePhysicalPress", &initial, &consumed);

    set_focus(&mut app, window_a, window_b, false);
    let consumed_transferred = project(&app, physical, window_a, window_b, consumed_witness);
    require_transition("TransferFocusWhileHeld", &consumed, &consumed_transferred);

    app.route_physical_repeat(window_b, physical);
    let consumed_repeated_witness = consumed_witness.silent_repeat();
    let consumed_repeated = project(
        &app,
        physical,
        window_a,
        window_b,
        consumed_repeated_witness,
    );
    require_transition(
        "RepeatOfConsumedPress",
        &consumed_transferred,
        &consumed_repeated,
    );

    app.release_physical_press(window_b, physical);
    assert_eq!(
        take_physical_release_trace(),
        Some(PhysicalReleaseTrace::Consumed {
            arrival_window: window_b,
            press_window: window_a,
        }),
        "consumed cross-window release returns before any PTY egress"
    );
    let consumed_released_witness = consumed_repeated_witness.released(window_b, None, false);
    let consumed_released = project(
        &app,
        physical,
        window_a,
        window_b,
        consumed_released_witness,
    );
    require_transition(
        "ReleaseConsumedPress",
        &consumed_repeated,
        &consumed_released,
    );
    assert!(!app.physical_press_owners.contains_key(&physical));
    assert!(
        !app.windows[&window_a]
            .consumed_press_keys
            .contains(&physical)
    );

    // Forwarded A/session-A press -> focus B/session-B -> release through B.
    // Kitty event types on A make the sentinel sink's Failed delivery prove
    // the release was encoded and attempted, not a silent legacy no-op.
    let (mut app, window_a, window_b, session_a, session_b) = two_window_app();
    let physical = PhysicalKey::Code(KeyCode::KeyB);
    let key = TKey::Named(NamedKey::ArrowUp);
    let mods = Modifiers::SHIFT;
    let base_layout = Some('w');
    term_lock(&app.pool.get(session_a).expect("session A").term).process(b"\x1b[>2u");
    let initial = project(
        &app,
        physical,
        window_a,
        window_b,
        ProjectionWitness::initial(),
    );

    app.note_forwarded_press_key(window_a, physical, false, key.clone(), mods, base_layout);
    match app.physical_press_owners.get(&physical) {
        Some(PhysicalPressOwner::Forwarded {
            window,
            session,
            key: stored_key,
            mods: stored_mods,
            base_layout: stored_layout,
        }) => {
            assert_eq!(*window, window_a);
            assert_eq!(*session, session_a);
            assert_eq!(stored_key, &key);
            assert_eq!(*stored_mods, mods);
            assert_eq!(*stored_layout, base_layout);
        }
        other => panic!("real forwarded owner missing/corrupt: {other:?}"),
    }
    let forwarded_witness = ProjectionWitness::held(Disposition::Forwarded, window_a);
    let forwarded = project(&app, physical, window_a, window_b, forwarded_witness);
    require_transition("ForwardPress", &initial, &forwarded);

    set_focus(&mut app, window_a, window_b, false);
    let forwarded_transferred = project(&app, physical, window_a, window_b, forwarded_witness);
    require_transition("TransferFocusWhileHeld", &forwarded, &forwarded_transferred);

    app.route_physical_repeat(window_b, physical);
    let repeat_trace = take_physical_release_trace().expect("forwarded repeat trace");
    let PhysicalReleaseTrace::Forwarded {
        arrival_window,
        press_window,
        session,
        key: routed_key,
        mods: routed_mods,
        base_layout: routed_layout,
        event_type,
        delivery,
    } = repeat_trace
    else {
        panic!("forwarded owner cannot repeat as consumed")
    };
    assert_eq!(arrival_window, window_b);
    assert_eq!(press_window, window_a);
    assert_eq!(session, session_a);
    assert_ne!(session, session_b, "repeat must not route to focused B");
    assert_eq!(routed_key, key);
    assert_eq!(routed_mods, mods);
    assert_eq!(routed_layout, base_layout);
    assert_eq!(event_type, aterm_types::keyboard::KeyEventType::Repeat);
    assert_eq!(
        delivery,
        crate::input::Delivery::Failed,
        "Kitty repeat was encoded against original session A's sentinel sink"
    );
    assert!(
        app.physical_press_owners.contains_key(&physical),
        "repeat must retain ownership for the final release"
    );
    let forwarded_repeated_witness = forwarded_witness.repeated(window_a);
    let forwarded_repeated = project(
        &app,
        physical,
        window_a,
        window_b,
        forwarded_repeated_witness,
    );
    require_transition(
        "ForwardRepeatOfForwardedPress",
        &forwarded_transferred,
        &forwarded_repeated,
    );

    app.release_physical_press(window_b, physical);
    let trace = take_physical_release_trace().expect("forwarded release trace");
    let PhysicalReleaseTrace::Forwarded {
        arrival_window,
        press_window,
        session,
        key: routed_key,
        mods: routed_mods,
        base_layout: routed_layout,
        event_type,
        delivery,
    } = trace
    else {
        panic!("forwarded owner cannot settle as consumed")
    };
    assert_eq!(arrival_window, window_b);
    assert_eq!(press_window, window_a);
    assert_eq!(session, session_a);
    assert_ne!(session, session_b, "release must not route to focused B");
    assert_eq!(routed_key, key);
    assert_eq!(routed_mods, mods);
    assert_eq!(routed_layout, base_layout);
    assert_eq!(event_type, aterm_types::keyboard::KeyEventType::Release);
    assert_eq!(
        delivery,
        crate::input::Delivery::Failed,
        "Kitty release was encoded against original session A's sentinel sink"
    );

    let forwarded_released_witness =
        forwarded_repeated_witness.released(window_b, Some(window_a), true);
    let forwarded_released = project(
        &app,
        physical,
        window_a,
        window_b,
        forwarded_released_witness,
    );
    require_transition(
        "ReleaseForwardedPress",
        &forwarded_repeated,
        &forwarded_released,
    );
    assert!(!app.physical_press_owners.contains_key(&physical));

    // A configured raw sequence has different release semantics (silent),
    // but its repeated bytes obey the same immutable cross-focus target.
    let (mut raw_app, raw_window_a, raw_window_b, raw_session_a, raw_session_b) = two_window_app();
    let raw_physical = PhysicalKey::Code(KeyCode::KeyC);
    let raw_bytes = b"\x1b[99~".to_vec();
    let raw_initial = project(
        &raw_app,
        raw_physical,
        raw_window_a,
        raw_window_b,
        ProjectionWitness::initial(),
    );
    raw_app.forward_literal_press(
        raw_window_a,
        raw_physical,
        InputEvent::KeySequence(raw_bytes.clone()),
    );
    assert_eq!(
        take_physical_release_trace(),
        Some(PhysicalReleaseTrace::Literal {
            arrival_window: raw_window_a,
            press_window: raw_window_a,
            session: raw_session_a,
            event: InputEvent::KeySequence(raw_bytes.clone()),
            repeated: false,
            delivery: crate::input::Delivery::Failed,
        }),
        "initial literal sequence is pinned to session A"
    );
    let raw_witness = ProjectionWitness::held(Disposition::Literal, raw_window_a);
    let raw_held = project(
        &raw_app,
        raw_physical,
        raw_window_a,
        raw_window_b,
        raw_witness,
    );
    require_transition("ForwardLiteralPress", &raw_initial, &raw_held);

    set_focus(&mut raw_app, raw_window_a, raw_window_b, false);
    let raw_transferred = project(
        &raw_app,
        raw_physical,
        raw_window_a,
        raw_window_b,
        raw_witness,
    );
    require_transition("TransferFocusWhileHeld", &raw_held, &raw_transferred);
    raw_app.route_physical_repeat(raw_window_b, raw_physical);
    assert_eq!(
        take_physical_release_trace(),
        Some(PhysicalReleaseTrace::Literal {
            arrival_window: raw_window_b,
            press_window: raw_window_a,
            session: raw_session_a,
            event: InputEvent::KeySequence(raw_bytes),
            repeated: true,
            delivery: crate::input::Delivery::Failed,
        }),
        "raw repeat must use original session/bytes"
    );
    assert_ne!(
        raw_session_a, raw_session_b,
        "raw test needs distinct destinations"
    );
    let raw_repeated_witness = raw_witness.repeated(raw_window_a);
    let raw_repeated = project(
        &raw_app,
        raw_physical,
        raw_window_a,
        raw_window_b,
        raw_repeated_witness,
    );
    require_transition(
        "ForwardRepeatOfLiteralPress",
        &raw_transferred,
        &raw_repeated,
    );
    assert!(raw_app.take_literal_release(raw_window_b, raw_physical));
    assert_eq!(
        take_physical_release_trace(),
        Some(PhysicalReleaseTrace::Consumed {
            arrival_window: raw_window_b,
            press_window: raw_window_a,
        }),
        "literal sequence release is byte-silent"
    );
    let raw_released_witness = raw_repeated_witness.released(raw_window_b, None, false);
    let raw_released = project(
        &raw_app,
        raw_physical,
        raw_window_a,
        raw_window_b,
        raw_released_witness,
    );
    require_transition("ReleaseLiteralPress", &raw_repeated, &raw_released);

    // Repeatable GUI actions are also bound to their press window. A real
    // font-zoom hold crosses focus A→B without becoming B's key episode.
    let (mut local_app, local_a, local_b, _, _) = two_window_app();
    let local_physical = PhysicalKey::Code(KeyCode::KeyD);
    let local_initial = project(
        &local_app,
        local_physical,
        local_a,
        local_b,
        ProjectionWitness::initial(),
    );
    local_app.note_local_repeat_press(
        local_a,
        local_physical,
        crate::LocalRepeatAction::FontZoom(crate::FontZoomRepeatAction::Increase),
    );
    let local_witness = ProjectionWitness::held(Disposition::Local, local_a);
    let local_held = project(&local_app, local_physical, local_a, local_b, local_witness);
    require_transition("CaptureLocalRepeatPress", &local_initial, &local_held);
    set_focus(&mut local_app, local_a, local_b, false);
    let local_transferred = project(&local_app, local_physical, local_a, local_b, local_witness);
    require_transition("TransferFocusWhileHeld", &local_held, &local_transferred);
    let font_before = local_app.font_px;
    local_app.route_physical_repeat(local_b, local_physical);
    assert!(
        local_app.font_px > font_before,
        "captured zoom repeat applied"
    );
    assert_eq!(
        take_physical_release_trace(),
        Some(PhysicalReleaseTrace::Local {
            arrival_window: local_b,
            press_window: local_a,
        })
    );
    let local_repeated_witness = local_witness.repeated(local_a);
    let local_repeated = project(
        &local_app,
        local_physical,
        local_a,
        local_b,
        local_repeated_witness,
    );
    require_transition("ForwardLocalRepeat", &local_transferred, &local_repeated);
    assert!(local_app.take_local_repeat_release(local_b, local_physical));
    let local_released_witness = local_repeated_witness.released(local_b, None, false);
    let local_released = project(
        &local_app,
        local_physical,
        local_a,
        local_b,
        local_released_witness,
    );
    require_transition("ReleaseLocalRepeatPress", &local_repeated, &local_released);

    // Non-vacuity: a resolver that sends both repeats and the release to
    // current focus B. Each mutant must violate its target invariant and be
    // rejected as a transition by the healthy derived model used above.
    let buggy = aterm_spec::interp::with_buggy(&model, 1);
    let orphan_repeat = buggy
        .successors("SwallowUntrackedRepeat", &untracked_initial)
        .into_iter()
        .next()
        .expect("Buggy untracked repeat transition");
    assert_eq!(orphan_repeat["repeat_emitted"], 1);
    assert!(!buggy.check_invariant("NoOrphanCsiUBytes", &orphan_repeat));
    let (accepted, out) =
        validate_transition("SwallowUntrackedRepeat", &untracked_initial, &orphan_repeat);
    assert!(
        !accepted,
        "NEGATIVE CONTROL: orphan untracked repeat must be rejected\n{out}"
    );

    // A release-time chord re-lookup, replayed on the real tables. Bind the
    // literal press's bytes to `ctrl+c` in a real `[key_sequences]` table:
    // with Ctrl let go mid-hold, re-asking the table with the release's
    // modifiers misses and falls through to the encoder, which owes a Kitty
    // release report for a press the PTY never saw. The local-repeat half
    // is the zoom hold's `ctrl+=` re-looked-up in `[keybindings]` with Ctrl
    // let go. Each real miss, applied to the real released state, is the
    // `Buggy=1` release, and the healthy model refuses it.
    let ctrl = winit::keyboard::ModifiersState::CONTROL;
    let none = winit::keyboard::ModifiersState::empty();
    let sequences = crate::keybinding::KeySequences::from_config(Some(
        &[("ctrl+c".to_string(), "\\e[99~".to_string())]
            .into_iter()
            .collect(),
    ));
    let c = winit::keyboard::Key::Character("c".into());
    assert_eq!(
        sequences.lookup(&c, ctrl),
        Some(&b"\x1b[99~"[..]),
        "the press's chord sends the literal bytes"
    );
    let bindings = crate::keybinding::Keybindings::from_config(Some(
        &[("ctrl+=".to_string(), "font_increase".to_string())]
            .into_iter()
            .collect(),
    ));
    let equals = winit::keyboard::Key::Character("=".into());
    assert_eq!(
        bindings.lookup(&equals, ctrl),
        Some(crate::keybinding::Action::FontIncrease),
        "the press's chord is the zoom hold"
    );
    for (release, from, released, swallowed_at_release, law) in [
        (
            "ReleaseLiteralPress",
            &raw_repeated,
            &raw_released,
            sequences.lookup(&c, none).is_some(),
            "LiteralInputRetainsSilentReleaseOwnership",
        ),
        (
            "ReleaseLocalRepeatPress",
            &local_repeated,
            &local_released,
            bindings.lookup(&equals, none).is_some(),
            "LocalRepeatRetainsSilentReleaseOwnership",
        ),
    ] {
        assert!(
            !swallowed_at_release,
            "{release}: a modifier let go mid-hold misses the chord"
        );
        let mut pre_fix = released.clone();
        pre_fix.insert("release_emitted", i64::from(!swallowed_at_release));
        pre_fix.insert("orphan_csi_u", i64::from(!swallowed_at_release));
        assert_eq!(buggy.successors(release, from), vec![pre_fix.clone()]);
        assert!(!buggy.check_invariant(law, &pre_fix), "{law}");
        let (accepted, out) = validate_transition(release, from, &pre_fix);
        assert!(
            !accepted,
            "NEGATIVE CONTROL: {release} leaking an orphan report must be rejected\n{out}"
        );
    }

    let repeat_misrouted = buggy
        .successors("ForwardRepeatOfForwardedPress", &forwarded_transferred)
        .into_iter()
        .next()
        .expect("Buggy cross-focus repeat transition");
    assert_eq!(repeat_misrouted["press_window"], 1);
    assert_eq!(repeat_misrouted["repeat_routed_window"], 2);
    assert!(!buggy.check_invariant("EmittedRepeatUsesOriginalPressTarget", &repeat_misrouted,));
    let (accepted, out) = validate_transition(
        "ForwardRepeatOfForwardedPress",
        &forwarded_transferred,
        &repeat_misrouted,
    );
    assert!(
        !accepted,
        "NEGATIVE CONTROL: current-focus repeat route must be rejected\n{out}"
    );

    let raw_repeat_misrouted = buggy
        .successors("ForwardRepeatOfLiteralPress", &raw_transferred)
        .into_iter()
        .next()
        .expect("Buggy cross-focus raw repeat transition");
    assert_eq!(raw_repeat_misrouted["press_window"], 1);
    assert_eq!(raw_repeat_misrouted["repeat_routed_window"], 2);
    assert!(!buggy.check_invariant(
        "EmittedRepeatUsesOriginalPressTarget",
        &raw_repeat_misrouted,
    ));
    let (accepted, out) = validate_transition(
        "ForwardRepeatOfLiteralPress",
        &raw_transferred,
        &raw_repeat_misrouted,
    );
    assert!(
        !accepted,
        "NEGATIVE CONTROL: current-focus raw repeat route must be rejected\n{out}"
    );

    let local_repeat_misrouted = buggy
        .successors("ForwardLocalRepeat", &local_transferred)
        .into_iter()
        .next()
        .expect("Buggy cross-focus local repeat transition");
    assert_eq!(local_repeat_misrouted["press_window"], 1);
    assert_eq!(local_repeat_misrouted["repeat_routed_window"], 2);
    assert!(!buggy.check_invariant(
        "EmittedRepeatUsesOriginalPressTarget",
        &local_repeat_misrouted,
    ));
    let (accepted, out) = validate_transition(
        "ForwardLocalRepeat",
        &local_transferred,
        &local_repeat_misrouted,
    );
    assert!(
        !accepted,
        "NEGATIVE CONTROL: current-focus local repeat route must be rejected\n{out}"
    );

    let misrouted = buggy
        .successors("ReleaseForwardedPress", &forwarded_repeated)
        .into_iter()
        .next()
        .expect("Buggy cross-focus release transition");
    assert_eq!(misrouted["press_window"], 1);
    assert_eq!(misrouted["release_arrival_window"], 2);
    assert_eq!(misrouted["release_routed_window"], 2);
    assert!(!buggy.check_invariant("ForwardedReleaseUsesOriginalPressTarget", &misrouted,));
    let (accepted, out) =
        validate_transition("ReleaseForwardedPress", &forwarded_repeated, &misrouted);
    assert!(
        !accepted,
        "NEGATIVE CONTROL: current-focus release route must be rejected\n{out}"
    );

    crate::logging::stderr_line!(
        "InputReleasePairing Tier-1: consumed, encoded, literal, and local A→focus-B \
         repeat/release routes conform; original identities preserved; mutants rejected."
    );
}

// The active machine has 21 actions. Twelve ownership/routing seams above carry
// real `#[refines]` anchors. These nine actions are explicit scope boundaries,
// not silent coverage holes.
#[aterm_spec::spec_unmodeled(
    machine = "input_release_pairing",
    action = "TransferFocusWhileHeld",
    reason = "OS/winit environment transition: Tier-1 drives real App::on_focus(A,false) then \
              App::on_focus(B,true) and validates that physical ownership is unchanged; no \
              key-owner mutator implements the focus arrival itself."
)]
#[aterm_spec::spec_unmodeled(
    machine = "input_release_pairing",
    action = "OpenOverlay",
    reason = "Orthogonal overlay environment step; cross-window physical-owner conformance \
              fixes overlays closed, while overlay_gate_pairing_tests drives the real open."
)]
#[aterm_spec::spec_unmodeled(
    machine = "input_release_pairing",
    action = "CloseOverlay",
    reason = "Orthogonal overlay environment step; cross-window physical-owner conformance \
              fixes overlays closed, while overlay_gate_pairing_tests drives the real close."
)]
#[aterm_spec::spec_unmodeled(
    machine = "input_release_pairing",
    action = "ConsumeOverlayPress",
    reason = "Controller/engine-key overlay ownership uses overlay_consumed_keys, not the \
              process-wide winit PhysicalKey owner map bound by this Tier-1 runner."
)]
#[aterm_spec::spec_unmodeled(
    machine = "input_release_pairing",
    action = "RepeatOfConsumedPress",
    reason = "Repeat is a read-only peek of the retained physical owner; dedicated shipping \
              tests prove it neither removes the owner nor emits bytes."
)]
#[aterm_spec::spec_unmodeled(
    machine = "input_release_pairing",
    action = "GateConsumesRepeatOfForwardedPress",
    reason = "Controller/engine-key repeats enter App::input without a winit PhysicalKey; \
              overlay_gate_pairing_tests drives the real pre-overlay press/open/repeat/release \
              seam. Physical repeats bypass current-content gates through route_physical_repeat."
)]
#[aterm_spec::spec_unmodeled(
    machine = "input_release_pairing",
    action = "PhysicalFocusLoss",
    reason = "Terminal OS epoch where no matching key-up is delivered. A focus transfer that \
              does deliver key-up is the separately validated TransferFocusWhileHeld action."
)]
#[aterm_spec::spec_unmodeled(
    machine = "input_release_pairing",
    action = "SettledRelease",
    reason = "Abstract terminal stutter after owner removal; no shipping state mutation exists."
)]
#[aterm_spec::spec_unmodeled(
    machine = "input_release_pairing",
    action = "SettledFocusEpoch",
    reason = "Abstract terminal stutter after an OS-cancelled epoch; no shipping mutation exists."
)]
#[expect(
    dead_code,
    reason = "carrier for the `spec_unmodeled` waivers above; nothing calls it"
)]
fn explicit_scope_waivers() {}
