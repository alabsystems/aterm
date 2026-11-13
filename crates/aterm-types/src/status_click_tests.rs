// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use super::*;

const fn key(kind: usize, flags: usize, at: f64, clicks: isize) -> EventKey {
    EventKey {
        kind,
        flags,
        at,
        clicks,
    }
}

const fn left(at: f64, clicks: isize) -> EventKey {
    key(LEFT_MOUSE_UP, 0, at, clicks)
}

const fn right(at: f64) -> EventKey {
    key(RIGHT_MOUSE_UP, 0, at, 1)
}

#[test]
fn a_right_release_or_a_control_click_opens_the_menu() {
    assert_eq!(classify(Some(left(1.0, 1)), None), Click::Primary);
    assert_eq!(classify(Some(right(1.0)), None), Click::Menu);
    assert_eq!(
        classify(Some(key(LEFT_MOUSE_UP, CONTROL, 1.0, 1)), None),
        Click::Menu
    );
    assert_eq!(
        classify(Some(key(RIGHT_MOUSE_UP, CONTROL, 1.0, 1)), None),
        Click::Menu
    );
    // Shift, Option and Command are no Control.
    let others = (1 << 17) | (1 << 19) | (1 << 20);
    assert_eq!(
        classify(Some(key(LEFT_MOUSE_UP, others, 1.0, 1)), None),
        Click::Primary
    );
    assert_eq!(classify(None, None), Click::Primary);
}

#[test]
fn a_press_finding_a_stale_event_is_the_items_own_act() {
    let control = key(LEFT_MOUSE_UP, CONTROL, 9.0, 1);
    assert_eq!(classify(Some(right(1.0)), None), Click::Menu);
    assert_eq!(classify(Some(right(2.0)), Some(right(1.0))), Click::Menu);
    assert_eq!(classify(Some(control), Some(right(1.0))), Click::Menu);
    assert_eq!(classify(Some(right(1.0)), Some(right(1.0))), Click::Primary);
    assert_eq!(classify(Some(control), Some(control)), Click::Primary);
    assert_eq!(classify(None, Some(right(1.0))), Click::Primary);
}

#[test]
fn only_a_fresh_left_release_counted_twice_or_more_is_a_repeat() {
    assert!(!repeated(Some(left(1.0, 1)), None));
    assert!(repeated(Some(left(1.1, 2)), Some(left(1.0, 1))));
    assert!(repeated(Some(left(1.2, 3)), Some(left(1.1, 2))));
    // The same event read again is no press of its own.
    assert!(!repeated(Some(left(1.1, 2)), Some(left(1.1, 2))));
    // A right release opens the menu however AppKit counts it, and no event is no repeat.
    assert!(!repeated(Some(key(RIGHT_MOUSE_UP, 0, 1.0, 2)), None));
    assert!(!repeated(None, Some(left(1.0, 2))));
    // An event that is no mouse button's carries no count.
    assert!(!repeated(Some(key(10, 0, 1.0, 0)), None));
}

/// CONTROL: the same left release is pointed when it is fresh and plain.
#[test]
fn only_a_fresh_plain_left_release_is_pointed() {
    assert!(pointed(Some(left(1.0, 1)), None));
    assert!(pointed(Some(left(1.1, 2)), Some(left(1.0, 1))));
    // No event, the event already read, a key's (`NSEventTypeKeyDown`), a right release and
    // a Control-click are not.
    assert!(!pointed(None, None));
    assert!(!pointed(None, Some(left(1.0, 1))));
    assert!(!pointed(Some(left(1.0, 1)), Some(left(1.0, 1))));
    assert!(!pointed(Some(key(10, 0, 1.0, 0)), None));
    assert!(!pointed(Some(right(1.0)), None));
    assert!(!pointed(Some(key(LEFT_MOUSE_UP, CONTROL, 1.0, 1)), None));
}
