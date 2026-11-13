// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use super::is_focus_replayed_press;
use aterm_window::event::{DeviceId, ElementState, KeyEvent, WindowEvent};
use aterm_window::keyboard::{Key, KeyCode, KeyLocation, NamedKey, PhysicalKey, SmolStr};

fn key(state: ElementState, is_synthetic: bool) -> WindowEvent {
    WindowEvent::KeyboardInput {
        device_id: DeviceId::dummy(),
        event: KeyEvent::synthetic_for_test(
            PhysicalKey::Code(KeyCode::Enter),
            Key::Named(NamedKey::Enter),
            Some(SmolStr::new("\r")),
            KeyLocation::Standard,
            state,
            false,
        ),
        is_synthetic,
    }
}

#[test]
fn only_a_press_replayed_on_focus_is_dropped() {
    for (state, is_synthetic, dropped) in [
        (ElementState::Pressed, true, true),
        (ElementState::Pressed, false, false),
        (ElementState::Released, true, false),
        (ElementState::Released, false, false),
    ] {
        assert_eq!(
            is_focus_replayed_press(&key(state, is_synthetic)),
            dropped,
            "{state:?} is_synthetic={is_synthetic}"
        );
    }
    assert!(!is_focus_replayed_press(&WindowEvent::Focused(true)));
}
