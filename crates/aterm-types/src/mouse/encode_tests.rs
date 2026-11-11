// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0
// Author: Andrew Yates

use super::*;

#[test]
fn a_thumb_buttons_legacy_release_is_button_three() {
    let back = 128;
    assert_eq!(
        encode_mouse(back, 10, 5, MouseEncoding::X10, true),
        [0x1b, b'[', b'M', 32 + 3, 43, 38]
    );
    assert_eq!(
        encode_mouse(back | 16, 10, 5, MouseEncoding::X10, true),
        [0x1b, b'[', b'M', 32 + 16 + 3, 43, 38]
    );
    assert_eq!(
        encode_mouse(back, 10, 5, MouseEncoding::Urxvt, true),
        b"\x1b[35;11;6M"
    );
    assert_eq!(
        encode_mouse(back, 10, 5, MouseEncoding::Sgr, true),
        b"\x1b[<128;11;6m"
    );
    assert_eq!(
        encode_mouse(back, 10, 5, MouseEncoding::X10, false),
        [0x1b, b'[', b'M', 32 + 128, 43, 38]
    );
}

#[test]
fn utf8_large_button_code() {
    let result = encode_utf8(144, 10, 5);
    assert_eq!(result[0..3], [0x1b, b'[', b'M']);
    assert_eq!(result[3], 0xC2);
    assert_eq!(result[4], 0xB0);
}

#[test]
fn encode_mouse_dispatches_sgr_pixel() {
    let result = encode_mouse(0, 10, 5, MouseEncoding::SgrPixel, false);
    assert_eq!(result, encode_sgr(0, 10, 5, false));
}

#[test]
fn u16_max_coordinates_never_overflow() {
    assert_eq!(
        encode_sgr(0, u16::MAX, u16::MAX, false),
        b"\x1b[<0;65535;65535M"
    );
    assert_eq!(
        encode_urxvt(32, u16::MAX, u16::MAX),
        b"\x1b[32;65535;65535M"
    );
    let x10 = encode_x10(0, u16::MAX, u16::MAX);
    assert_eq!((x10[4], x10[5]), (255, 255));
}
