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

/// A line of scroll, a touch that reaches a window whether or not it is focused.
pub(crate) fn scroll() -> WindowEvent {
    WindowEvent::MouseWheel {
        device_id: DeviceId::dummy(),
        delta: MouseScrollDelta::LineDelta(0.0, -1.0),
        phase: TouchPhase::Moved,
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

/// A window is attended until the whole of its attention has passed since the last touch and
/// not a millisecond longer; the touch that ends a lapse says so; `None` is always attended.
/// Output (a stir) attends nothing.
#[test]
fn a_window_is_attended_for_its_attention_after_a_touch_and_output_attends_nothing() {
    use super::ATTENTION;
    let t0 = Instant::now();
    let mut rest = Rest::new(t0);
    assert!(rest.attended(), "a new window");
    assert!(!rest.settle_attended(t0 + ATTENTION - Duration::from_millis(1), Some(ATTENTION)));
    assert!(rest.attended(), "a millisecond short of its attention");
    assert!(
        !rest.stir(t0 + ATTENTION - Duration::from_millis(1)),
        "output"
    );
    assert!(
        rest.settle_attended(t0 + ATTENTION, Some(ATTENTION)),
        "the output a millisecond ago attended nothing"
    );
    assert!(!rest.attended());
    assert!(!rest.settle_attended(t0 + ATTENTION * 2, Some(ATTENTION)));
    assert!(rest.settle_attended(t0 + ATTENTION * 2, None), "no bound");
    assert!(rest.attended());
    assert!(rest.settle_attended(t0 + ATTENTION * 2, Some(ATTENTION)));

    let touched = t0 + ATTENTION * 3;
    assert!(rest.touch(touched, true), "the touch ends the lapse");
    assert!(rest.attended(), "at once, before any settle");
    assert!(!rest.touch(touched, true), "a second touch ends none");
    assert!(
        !rest.settle_attended(
            touched + ATTENTION - Duration::from_millis(1),
            Some(ATTENTION)
        ),
        "its attention starts over at the touch"
    );
    assert!(rest.settle_attended(touched + ATTENTION, Some(ATTENTION)));
}

/// The `Resized` that reports the size aterm itself asked for is its own, each time it
/// arrives; a resize to another size is not, and forgets the size asked, so a drag away from
/// that size and back is a person's at every step. No other event is aterm's own.
#[test]
fn only_the_resize_aterm_asked_for_is_its_own() {
    use aterm_window::dpi::PhysicalSize;
    let resized = |w, h| WindowEvent::Resized(PhysicalSize::new(w, h));
    let mut rest = Rest::new(Instant::now());
    assert!(!rest.own(&resized(800, 600)), "nothing was asked");
    rest.own_resize((800, 600));
    assert!(!rest.own(&key()), "a key");
    assert!(!rest.own(&WindowEvent::Focused(true)), "the focus arriving");
    assert!(rest.own(&resized(800, 600)), "the size asked");
    assert!(rest.own(&resized(800, 600)), "reported again");
    assert!(!rest.own(&resized(801, 600)), "a drag away from it");
    assert!(!rest.own(&resized(800, 600)), "and back");
}

/// An unattended window's deck holds still whatever reading lands; after the touch that
/// attends a focused window again it holds until a reading lands after that touch. The touch
/// that attends an unfocused window again says it ended the lapse and holds nothing: a reading
/// that landed before it moves the deck once the window is focused.
///
/// Negative control, run once by hand and restored by copy: `Rest::touch` holding the deck on
/// every touch that ends a lapse, focused or not (`if lapsed` in place of `if lapsed &&
/// focused` before it sets `woke_at`), fails "an unfocused window's touch holds nothing".
#[test]
fn an_unattended_window_holds_its_deck_until_a_reading_lands_after_the_next_touch() {
    use super::ATTENTION;
    let t0 = Instant::now();
    let mut rest = Rest::new(t0);
    assert!(rest.deck_moves(Some(t0)));
    assert!(rest.settle_attended(t0 + ATTENTION, Some(ATTENTION)));
    assert!(
        !rest.deck_moves(Some(t0 + ATTENTION + Duration::from_secs(1))),
        "unattended, whatever lands"
    );
    assert!(
        !rest.stir(t0 + ATTENTION + Duration::from_secs(2)),
        "output"
    );
    assert!(
        !rest.deck_moves(Some(t0 + ATTENTION + Duration::from_secs(3))),
        "output brings no motion back"
    );
    let touched = t0 + ATTENTION * 2;
    assert!(rest.touch(touched, true));
    assert!(!rest.deck_moves(Some(touched)), "the touch's own frame");
    assert!(
        rest.deck_moves(Some(touched + Duration::from_millis(300))),
        "the reading after the touch"
    );
    assert!(!rest.touch(touched + Duration::from_secs(1), true));
    assert!(
        rest.deck_moves(Some(touched + Duration::from_millis(300))),
        "a touch of an attended window holds nothing"
    );

    let read = touched + Duration::from_millis(300);
    assert!(rest.settle_attended(touched + ATTENTION * 2, Some(ATTENTION)));
    let scrolled = touched + ATTENTION * 2;
    assert!(
        rest.touch(scrolled, false),
        "an unfocused window's touch ends the lapse"
    );
    assert!(rest.attended());
    assert!(
        rest.deck_moves(Some(read)),
        "an unfocused window's touch holds nothing"
    );
}
