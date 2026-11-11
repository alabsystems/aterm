// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use super::*;

#[test]
fn requests_are_encoded_as_the_protocols_lay_them_out() {
    let q = query_extension("XTEST");
    assert_eq!(&q[..8], [98, 0, 4, 0, 5, 0, 0, 0]);
    assert_eq!(&q[8..13], b"XTEST");
    assert_eq!(q.len(), 16, "padded to a word");
    assert_eq!(get_keyboard_mapping(8, 248), [101, 0, 2, 0, 8, 248, 0, 0]);
    assert_eq!(get_input_focus(), [43, 0, 1, 0]);
    let f = fake_input(132, MOTION_NOTIFY, 1, -40, 30);
    assert_eq!(f.len(), 36, "FakeInput is nine words");
    assert_eq!(&f[..6], [132, FAKE_INPUT, 9, 0, MOTION_NOTIFY, 1]);
    assert_eq!(u32_at(&f, 8), Some(0), "time: now");
    assert_eq!(u32_at(&f, 12), Some(0), "root: the pointer's screen");
    assert_eq!(i16::from_le_bytes([f[24], f[25]]), -40, "rootX");
    assert_eq!(u16_at(&f, 26), Some(30), "rootY");
    assert_eq!(f[35], 0, "deviceid: the core devices");
    let k = fake_input(132, KEY_PRESS, 48, 0, 0);
    assert_eq!(&k[4..6], [KEY_PRESS, 48]);
}

#[test]
fn a_keysym_is_found_on_a_keys_first_level_only() {
    let mut reply = vec![1, 2, 0, 0, 8, 0, 0, 0];
    reply.resize(32, 0);
    for s in [0x61u32, 0x41, 0xfe51, 0xfe57, 0, 0x27, 0x61, 0x41] {
        reply.extend_from_slice(&s.to_le_bytes());
    }
    let map = Keymap::parse(&reply, 8, 4).expect("parses");
    assert_eq!(map.keycode(0x61), Some(8), "the lowest of two");
    assert_eq!(map.keycode(0xfe51), Some(9));
    assert_eq!(map.keycode(0x27), None, "a second level is not pressed");
    assert_eq!(map.keycode(0xffff), None);
    assert!(Keymap::parse(&reply, 8, 5).is_err(), "short of a fifth key");
    assert!(Keymap::parse(&reply[..1], 8, 1).is_err());
    let mut none = reply.clone();
    none[1] = 0;
    assert!(Keymap::parse(&none, 8, 4).is_err(), "no keysym per keycode");
}

#[test]
fn what_the_server_holds_down_is_read_bit_by_bit() {
    assert_eq!(held_buttons(0x0501), [1, 3]);
    assert_eq!(held_buttons(0x00ff), Vec::<u8>::new(), "modifiers only");
    let mut bits = [0u8; 32];
    bits[4] = 0b0100_0001; // keycodes 32 and 38
    bits[31] = 0x80; // keycode 255
    assert_eq!(held_keys(&bits), [32, 38, 255]);
    assert_eq!(held_keys(&[0; 32]), Vec::<u8>::new());
    assert_eq!(query_pointer(0x0505), [38, 0, 2, 0, 5, 5, 0, 0]);
    assert_eq!(query_keymap(), [44, 0, 1, 0]);
}

#[test]
fn evdev_buttons_are_x_buttons() {
    assert_eq!(x_button(0x110), Some(1));
    assert_eq!(x_button(0x112), Some(2));
    assert_eq!(x_button(0x111), Some(3));
    assert_eq!(x_button(0x113), None);
}
