// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0
// Author: Andrew Yates

use super::*;

#[test]
fn button_slots_are_dense_and_distinct() {
    let slots: Vec<u8> = [
        MouseButton::Left,
        MouseButton::Middle,
        MouseButton::Right,
        MouseButton::Back,
        MouseButton::Forward,
    ]
    .iter()
    .map(|b| b.slot())
    .collect();
    assert_eq!(slots, vec![0, 1, 2, 3, 4]);
    assert_eq!(MouseButton::Back.code() & 7, MouseButton::Left.code() & 7);
}

#[test]
fn only_vertical_wheel_directions_carry_a_viewport_sense() {
    assert_eq!(WheelDir::Up.vertical_up(), Some(true));
    assert_eq!(WheelDir::Down.vertical_up(), Some(false));
    assert_eq!(WheelDir::Left.vertical_up(), None);
    assert_eq!(WheelDir::Right.vertical_up(), None);
    assert!(!WheelDir::Up.is_horizontal());
    assert!(!WheelDir::Down.is_horizontal());
    assert!(WheelDir::Left.is_horizontal());
    assert!(WheelDir::Right.is_horizontal());
}
