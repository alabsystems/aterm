// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use super::{Rest, stirs};
use aterm_window::event::{
    DeviceId, ElementState, Ime, KeyEvent, Modifiers, MouseButton, MouseScrollDelta, TouchPhase,
    WindowEvent,
};
use aterm_window::keyboard::{Key, KeyCode, KeyLocation, PhysicalKey, SmolStr};
use std::time::{Duration, Instant};

/// The quiet is whole at `after` and not a millisecond before; the first stir wakes the
/// window at once and says so, and a stir of an awake window says nothing woke.
#[test]
fn a_window_rests_after_the_whole_quiet_and_wakes_at_the_first_stir() {
    let t0 = Instant::now();
    let after = Duration::from_secs(30);
    let mut rest = Rest::new(t0);
    assert!(!rest.settle(t0 + after - Duration::from_millis(1), Some(after)));
    assert!(!rest.resting(), "a millisecond short of the quiet");
    assert!(rest.settle(t0 + after, Some(after)), "the quiet is whole");
    assert!(rest.resting());
    assert!(!rest.settle(t0 + after * 2, Some(after)), "it rests on");

    let woke_at = t0 + after * 2;
    assert!(rest.stir(woke_at), "the first stir wakes it");
    assert!(!rest.resting(), "awake at once, before any settle");
    assert!(!rest.stir(woke_at), "a second stir wakes nothing");
    assert!(
        !rest.settle(woke_at + after - Duration::from_millis(1), Some(after)),
        "the quiet starts over at the stir"
    );
    assert!(rest.settle(woke_at + after, Some(after)));
}

/// A deck moves in a window that never rested and stops while it rests; after a wake it holds
/// still until a reading lands after the wake, and a reading from before the wake moves
/// nothing.
#[test]
fn a_woken_window_holds_its_deck_until_a_reading_lands_after_the_wake() {
    let t0 = Instant::now();
    let after = Duration::from_secs(30);
    let mut rest = Rest::new(t0);
    let read_before = t0 + Duration::from_secs(1);
    assert!(rest.deck_moves(None), "a window that never rested");
    assert!(rest.deck_moves(Some(read_before)));
    assert!(
        !rest.stir(t0 + Duration::from_secs(2)),
        "an awake stir wakes nothing"
    );
    assert!(rest.deck_moves(Some(read_before)), "nor holds its deck");
    assert!(rest.settle(t0 + after * 2, Some(after)));
    assert!(!rest.deck_moves(Some(t0 + after * 2)), "resting");
    let woke_at = t0 + after * 3;
    assert!(rest.stir(woke_at));
    assert!(!rest.deck_moves(Some(read_before)), "the wake's own frame");
    assert!(
        !rest.deck_moves(Some(woke_at)),
        "a reading no later than the wake"
    );
    assert!(!rest.deck_moves(None));
    assert!(
        rest.deck_moves(Some(woke_at + Duration::from_millis(300))),
        "the reading the wake asked for"
    );
}

/// `None` (`idle_rest_secs = 0`) never rests, and wakes a window that rested under the old
/// setting, so a reload applies at the next wake.
#[test]
fn no_rest_time_never_rests_and_wakes_a_resting_window() {
    let t0 = Instant::now();
    let mut rest = Rest::new(t0);
    assert!(!rest.settle(t0 + Duration::from_secs(86_400), None));
    assert!(!rest.resting());
    assert!(rest.settle(t0 + Duration::from_secs(2), Some(Duration::from_secs(2))));
    assert!(
        rest.settle(t0 + Duration::from_secs(3), None),
        "a reload to never wakes it"
    );
    assert!(!rest.resting());
}

/// A press of the `a` key, the event other modules' tests stir a window with.
pub(crate) fn key() -> WindowEvent {
    WindowEvent::KeyboardInput {
        device_id: DeviceId::dummy(),
        event: KeyEvent::synthetic_for_test(
            PhysicalKey::Code(KeyCode::KeyA),
            Key::Character(SmolStr::new("a")),
            Some(SmolStr::new("a")),
            KeyLocation::Standard,
            ElementState::Pressed,
            false,
        ),
        is_synthetic: false,
    }
}

/// Every person's event stirs the window; a frame, an occlusion change, the focus leaving and
/// a theme change never do, so the window's own drawing cannot keep it awake.
#[test]
fn a_frame_or_an_occlusion_never_stirs_and_every_person_event_does() {
    let did = DeviceId::dummy();
    for (event, stirred) in [
        (WindowEvent::RedrawRequested, false),
        (WindowEvent::Occluded(true), false),
        (WindowEvent::Occluded(false), false),
        (WindowEvent::Focused(false), false),
        (
            WindowEvent::ThemeChanged(aterm_window::window::Theme::Dark),
            false,
        ),
        (key(), true),
        (WindowEvent::Ime(Ime::Commit("é".into())), true),
        (WindowEvent::ModifiersChanged(Modifiers::default()), true),
        (
            WindowEvent::CursorMoved {
                device_id: did,
                position: (4.0, 2.0).into(),
            },
            true,
        ),
        (
            WindowEvent::MouseInput {
                device_id: did,
                state: ElementState::Pressed,
                button: MouseButton::Left,
            },
            true,
        ),
        (
            WindowEvent::MouseWheel {
                device_id: did,
                delta: MouseScrollDelta::LineDelta(0.0, -1.0),
                phase: TouchPhase::Moved,
            },
            true,
        ),
        (WindowEvent::Focused(true), true),
        (WindowEvent::Resized((640, 480).into()), true),
        (WindowEvent::DroppedFile("notes.txt".into()), true),
    ] {
        assert_eq!(stirs(&event), stirred, "{event:?}");
    }
}
