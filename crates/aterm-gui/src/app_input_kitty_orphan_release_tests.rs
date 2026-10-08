// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use super::{PhysicalReleaseTrace, take_physical_release_trace};
use crate::{App, WindowId, term_lock};
use winit::keyboard::{KeyCode, PhysicalKey};

fn pairing_step(
    model: &aterm_spec::derive::Model,
    state: &mut aterm_spec::interp::State,
    action: &'static str,
) {
    assert!(model.fire(action, state), "{action}: {state:?}");
    for invariant in &model.invariants {
        assert!(
            model.check_invariant(invariant.name, state),
            "shipping tracker violates {} after {action}: {state:?}",
            invariant.name,
        );
    }
}

/// A key release whose press a GUI gate consumed (settings/search mode, a
/// keybinding `Action`, a Cmd/Super shortcut, a scrollback/pane/zoom chord,
/// an IME-suppressed key) is swallowed exactly once: `take_consumed_release`
/// removes the tracked physical key and returns `true`, so `on_key` returns
/// without reaching the seam encoder (no orphan Kitty `REPORT_EVENT_TYPES`
/// release report). A second release, or any untracked key, still encodes.
#[test]
fn consumed_press_release_is_swallowed_exactly_once() {
    let mut app = App::headless_for_test();
    let wid = WindowId(0);
    let key = PhysicalKey::Code(KeyCode::KeyA);
    let other = PhysicalKey::Code(KeyCode::KeyB);
    let model = aterm_spec::derive::input_release_pairing_model();
    let mut state = model.init_state();
    // No tracked press yet: a normal key's release is forwarded (encodes as today).
    assert!(
        !app.take_consumed_release(wid, key),
        "an untracked release is not swallowed"
    );
    assert!(model.successors("ReleaseConsumedPress", &state).is_empty());
    assert!(model.successors("ReleaseForwardedPress", &state).is_empty());
    // A GUI gate consumed this physical key's PRESS.
    app.note_consumed_press_key(wid, key, false);
    pairing_step(&model, &mut state, "ConsumePhysicalPress");

    // Tier-1 non-vacuity: the removed branch emits a release after a consumed
    // press and violates both byte-silence and orphan-report obligations.
    let buggy = aterm_spec::interp::with_buggy(&model, 1);
    let mut mutant = state.clone();
    assert!(buggy.fire("ReleaseConsumedPress", &mut mutant));
    assert!(!buggy.check_invariant("NoOrphanCsiUBytes", &mutant));
    // The matching RELEASE is swallowed and its entry removed.
    assert!(
        app.take_consumed_release(wid, key),
        "the matching release is swallowed (no orphan release report)"
    );
    pairing_step(&model, &mut state, "ReleaseConsumedPress");
    assert!(
        app.windows
            .get(&wid)
            .unwrap()
            .consumed_press_keys
            .is_empty(),
        "the entry is removed on the first release"
    );
    // A second release for the SAME key is not swallowed (no stale double-swallow),
    // and an unrelated key is never affected.
    assert!(
        !app.take_consumed_release(wid, key),
        "no stale double-swallow"
    );
    assert!(
        !app.take_consumed_release(wid, other),
        "unrelated key untouched"
    );
}

/// Defensive field-level rule: even if a future caller presents a REPEAT to
/// the consumed-note seam, it cannot rewrite an existing forwarded episode.
/// Shipping `on_key` routes repeats before all live GUI gates.
#[test]
fn repeat_does_not_note_and_release_is_not_swallowed() {
    let mut app = App::headless_for_test();
    let wid = WindowId(0);
    let key = PhysicalKey::Code(KeyCode::PageUp);
    let model = aterm_spec::derive::input_release_pairing_model();
    let mut state = model.init_state();
    pairing_step(&model, &mut state, "ForwardPress");
    // Plain press fell through to the encoder. Any later repeat-note attempt
    // is a no-op, so the app remains owed the matching release.
    app.note_consumed_press_key(wid, key, true);
    app.note_consumed_press_key(wid, key, true);
    assert!(
        app.windows
            .get(&wid)
            .unwrap()
            .consumed_press_keys
            .is_empty(),
        "a repeat never records a consumed press"
    );
    assert!(
        !app.take_consumed_release(wid, key),
        "the release reaches the encoder — the app saw the press, it gets the release"
    );
    pairing_step(&model, &mut state, "ReleaseForwardedPress");
}

/// If the platform loses a key-up and later reports a fresh key-down for
/// the same physical key, the new press proves the old episode ended: the
/// old forwarded owner is retired through the exact-release path before the
/// new disposition is installed, or REPORT_EVENT_TYPES applications would
/// see a permanently held key.
#[test]
fn fresh_press_closes_stale_forwarded_episode_before_replacement() {
    use aterm_types::keyboard::{Key as TKey, Modifiers};

    let mut app = App::headless_for_test();
    let original_window = WindowId(0);
    let replacement_window = WindowId(777);
    let physical = PhysicalKey::Code(KeyCode::KeyA);
    let original_session = app.front_terminal(original_window).unwrap().session;
    app.note_forwarded_press_key(
        original_window,
        physical,
        false,
        TKey::Character('a'),
        Modifiers::SHIFT,
        Some('a'),
    );

    app.note_consumed_press_key(replacement_window, physical, false);
    assert!(matches!(
        app.physical_press_owners.get(&physical),
        Some(crate::PhysicalPressOwner::Consumed { window })
            if *window == replacement_window
    ));
    assert_eq!(
        take_physical_release_trace(),
        Some(PhysicalReleaseTrace::Forwarded {
            arrival_window: replacement_window,
            press_window: original_window,
            session: original_session,
            key: TKey::Character('a'),
            mods: Modifiers::SHIFT,
            base_layout: Some('a'),
            event_type: aterm_types::keyboard::KeyEventType::Release,
            delivery: crate::input::Delivery::Full,
        }),
        "the stale release uses old identity before new ownership installs"
    );
    assert!(app.take_consumed_release(replacement_window, physical));
}

/// A repeat pinned to a now-hidden session must update that terminal only;
/// it cannot heat or animate the replacement tab that happens to occupy the
/// press window. This is the presentation half of immutable routing.
#[test]
fn hidden_owned_repeat_does_not_arm_replacement_tab_effects() {
    use aterm_types::keyboard::{Key as TKey, Modifiers};

    let mut app = App::headless_for_test();
    let wid = WindowId(0);
    let physical = PhysicalKey::Code(KeyCode::KeyH);
    let original = app.front_terminal(wid).unwrap().session;
    term_lock(&app.pool.get(original).unwrap().term).process(b"\x1b[>2u");
    app.note_forwarded_press_key(
        wid,
        physical,
        false,
        TKey::Character('h'),
        Modifiers::empty(),
        Some('h'),
    );

    let replacement = app.next_session_id;
    app.push_stub_tab(wid, crate::stub_session(replacement));
    assert_eq!(app.front_terminal(wid).unwrap().session, replacement);
    app.windows.get_mut(&wid).unwrap().input_hot = false;
    assert!(!app.windows[&wid].fx.companions().cursor_cat.is_active());

    app.route_physical_repeat(wid, physical);
    assert!(
        !app.windows[&wid].input_hot,
        "hidden-session repeat must not mark replacement presentation hot"
    );
    assert!(
        !app.windows[&wid].fx.companions().cursor_cat.is_active(),
        "hidden-session repeat must not summon the cat in replacement tab"
    );
    assert_eq!(app.front_terminal(wid).unwrap().session, replacement);
    assert!(app.physical_press_owners.contains_key(&physical));
    let PhysicalReleaseTrace::Forwarded { session, .. } =
        take_physical_release_trace().expect("hidden repeat trace")
    else {
        panic!("encoded owner must stay encoded")
    };
    assert_eq!(session, original, "bytes remain pinned to hidden session A");
}
