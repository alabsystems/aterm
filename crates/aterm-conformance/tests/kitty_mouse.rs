// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates
//
// Kitty keyboard protocol and xterm mouse tracking: DECSET/CSI bytes fed
// through the engine, and the exact bytes it encodes back to the application.
// Specs: https://sw.kovidgoyal.net/kitty/keyboard-protocol/ and xterm ctlseqs
// "Mouse Tracking". `Screen` has no encoder accessors, so these drive
// `Terminal` directly.

use aterm_conformance::{Screen, run};
use aterm_core::terminal::Terminal;
use aterm_types::keyboard::{Key, KeyboardMode, Modifiers, NamedKey, encode_key};
use aterm_types::mouse::WheelDir;

/// Feed bytes to a raw 24x80 engine `Terminal`.
fn term_24x80(input: &[u8]) -> Terminal {
    let mut t = Terminal::new(24, 80);
    t.process(input);
    t
}

/// `CSI ? u` queries the enhancement flags (`CSI ? flags u`); every
/// enhancement is opt-in, so a fresh terminal reports 0.
#[test]
fn kitty_query_default_reports_zero_flags() {
    assert_eq!(run(b"\x1b[?u").response_string(), "\x1b[?0u");
}

/// `CSI > flags u` pushes onto the per-screen stack; `CSI < Ps u` pops Ps
/// entries (default 1). Flag 0b1 disambiguates, 0b100 reports alternate keys.
#[test]
fn kitty_push_query_pop_round_trip() {
    let mut s = Screen::new(24, 80);

    s.feed(b"\x1b[>1u\x1b[?u"); // push 1 (disambiguate)
    assert_eq!(s.response_string(), "\x1b[?1u", "after push 1");

    s.feed(b"\x1b[>5u\x1b[?u"); // push 5 (disambiguate|alternates) on top
    assert_eq!(s.response_string(), "\x1b[?5u", "after push 5");

    s.feed(b"\x1b[<u\x1b[?u"); // pop (count defaults to 1)
    assert_eq!(s.response_string(), "\x1b[?1u", "pop restores previous 1");

    s.feed(b"\x1b[<1u\x1b[?u"); // pop the last entry
    assert_eq!(s.response_string(), "\x1b[?0u", "stack empty -> flags 0");
}

/// A pop that empties the stack resets the flags; a pop deeper than the stack
/// is not an error.
#[test]
fn kitty_pop_on_empty_stack_is_clamped_no_underflow() {
    // Single pop with nothing pushed.
    assert_eq!(run(b"\x1b[<u\x1b[?u").response_string(), "\x1b[?0u");
    // Pop count far beyond any stack depth, after one real push.
    assert_eq!(
        run(b"\x1b[>1u\x1b[<100u\x1b[?u").response_string(),
        "\x1b[?0u",
        "over-large pop empties the stack and resets flags to 0"
    );
}

/// Flag 0b1 (disambiguate): Esc is `CSI 27 u` and Ctrl+C `CSI 99 ; 5 u`
/// (modifier field 1 + ctrl 4), not a C0 control.
#[test]
fn kitty_disambiguate_flag_switches_esc_and_ctrl_c_to_csi_u() {
    // Baseline: legacy protocol. Esc -> 0x1B, Ctrl+C -> 0x03 (C0 ETX).
    let t = term_24x80(b"");
    let mode = t.keyboard_mode();
    assert!(!mode.contains(KeyboardMode::DISAMBIGUATE_ESC_CODES));
    assert_eq!(
        encode_key(&Key::Named(NamedKey::Escape), Modifiers::empty(), mode),
        b"\x1b"
    );
    assert_eq!(
        encode_key(&Key::Character('c'), Modifiers::CTRL, mode),
        b"\x03"
    );

    // Push flag 1 (disambiguate) via wire bytes, then re-derive the mode.
    let t = term_24x80(b"\x1b[>1u");
    let mode = t.keyboard_mode();
    assert!(
        mode.contains(KeyboardMode::DISAMBIGUATE_ESC_CODES),
        "CSI > 1 u must set the engine's DISAMBIGUATE_ESC_CODES mode bit"
    );
    assert_eq!(
        encode_key(&Key::Named(NamedKey::Escape), Modifiers::empty(), mode),
        b"\x1b[27u",
        "kitty spec: Esc is reported as CSI 27 u under disambiguation"
    );
    assert_eq!(
        encode_key(&Key::Character('c'), Modifiers::CTRL, mode),
        b"\x1b[99;5u",
        "kitty spec: Ctrl+C is CSI 99;5 u (no C0 control) under disambiguation"
    );
}

/// Under disambiguate alone, keys with a legacy non-text encoding keep it:
/// ArrowUp stays `CSI A`.
#[test]
fn kitty_disambiguate_keeps_legacy_arrow_encoding() {
    let t = term_24x80(b"\x1b[>1u");
    let mode = t.keyboard_mode();
    assert_eq!(
        encode_key(&Key::Named(NamedKey::ArrowUp), Modifiers::empty(), mode),
        b"\x1b[A",
        "arrows retain legacy CSI form under disambiguate-only"
    );
}

/// Flag 0b1000 (report all keys as escape codes), set by `CSI = 8 ; 1 u`
/// (mode 1 sets exactly these bits): a plain 'a' is `CSI 97 u`, not 0x61.
#[test]
fn kitty_report_all_keys_as_esc_encodes_plain_text_key_as_csi_u() {
    let t = term_24x80(b"\x1b[=8;1u");
    let mode = t.keyboard_mode();
    assert!(mode.contains(KeyboardMode::REPORT_ALL_KEYS_AS_ESC));
    assert_eq!(
        encode_key(&Key::Character('a'), Modifiers::empty(), mode),
        b"\x1b[97u",
        "kitty spec: text keys become CSI <codepoint> u under flag 8"
    );
}

/// Popping the enhancement restores the legacy encoding: Esc is 0x1B again.
#[test]
fn kitty_pop_restores_legacy_encoding() {
    let t = term_24x80(b"\x1b[>1u\x1b[<u");
    let mode = t.keyboard_mode();
    assert!(!mode.contains(KeyboardMode::DISAMBIGUATE_ESC_CODES));
    assert_eq!(
        encode_key(&Key::Named(NamedKey::Escape), Modifiers::empty(), mode),
        b"\x1b"
    );
}

/// DECSET 1000 (xterm ctlseqs "Mouse Tracking"): `CSI M Cb Cx Cy` with
/// Cb = button + 32 (release is 3) and Cx/Cy = 1-based position + 32, so
/// 0-based (10, 5) is (43, 38).
#[test]
fn mouse_1000_default_encoding_press_release_exact_bytes() {
    let t = term_24x80(b"\x1b[?1000h");
    assert_eq!(
        t.encode_mouse_press(0, 10, 5, 0),
        Some(vec![0x1b, b'[', b'M', 32, 43, 38]),
        "left press: Cb=32+0, Cx=32+11, Cy=32+6"
    );
    assert_eq!(
        t.encode_mouse_release(0, 10, 5, 0),
        Some(vec![0x1b, b'[', b'M', 35, 43, 38]),
        "release: Cb=32+3 (button 3 = release in X10-style encoding)"
    );
}

/// Wheel up is button code 64 (Cb byte 96); shift adds 4.
#[test]
fn mouse_1000_wheel_up_is_button_64_and_shift_adds_4() {
    let t = term_24x80(b"\x1b[?1000h");
    assert_eq!(
        t.encode_mouse_wheel(WheelDir::Up, 10, 5, 0),
        Some(vec![0x1b, b'[', b'M', 96, 43, 38]),
        "wheel up: Cb = 32 + 64"
    );
    assert_eq!(
        t.encode_mouse_press(0, 10, 5, 4),
        Some(vec![0x1b, b'[', b'M', 36, 43, 38]),
        "shift+left press: Cb = 32 + (0|4)"
    );
}

/// The horizontal wheel is buttons 6 and 7, codes 66 and 67 (read under SGR).
#[test]
fn mouse_1006_horizontal_wheel_is_buttons_66_and_67() {
    let t = term_24x80(b"\x1b[?1000h\x1b[?1006h");
    assert_eq!(
        t.encode_mouse_wheel(WheelDir::Left, 10, 5, 0).as_deref(),
        Some(b"\x1b[<66;11;6M".as_slice()),
        "tilt left: button 6"
    );
    assert_eq!(
        t.encode_mouse_wheel(WheelDir::Right, 10, 5, 0).as_deref(),
        Some(b"\x1b[<67;11;6M".as_slice()),
        "tilt right: button 7"
    );
}

/// Buttons 8-11 start at code 128: back/forward are 128 and 129, and an SGR
/// release keeps the button and flips the final byte.
#[test]
fn mouse_1006_thumb_buttons_are_128_and_129() {
    use aterm_types::mouse::MouseButton;
    let t = term_24x80(b"\x1b[?1000h\x1b[?1006h");
    assert_eq!(
        t.encode_mouse_press(MouseButton::Back.code(), 10, 5, 0)
            .as_deref(),
        Some(b"\x1b[<128;11;6M".as_slice()),
        "thumb back press: xterm button 8"
    );
    assert_eq!(
        t.encode_mouse_release(MouseButton::Forward.code(), 10, 5, 0)
            .as_deref(),
        Some(b"\x1b[<129;11;6m".as_slice()),
        "thumb forward release keeps its identity under SGR"
    );
}

/// DECSET 1006 (SGR): `CSI < Cb ; Px ; Py M`, final `m` for release. No +32
/// offset, and the release keeps the button number.
#[test]
fn mouse_1006_sgr_press_release_keeps_button() {
    let t = term_24x80(b"\x1b[?1000h\x1b[?1006h");
    assert_eq!(
        t.encode_mouse_press(0, 10, 5, 0).as_deref(),
        Some(b"\x1b[<0;11;6M".as_slice())
    );
    assert_eq!(
        t.encode_mouse_release(0, 10, 5, 0).as_deref(),
        Some(b"\x1b[<0;11;6m".as_slice()),
        "SGR release: same button code, final 'm'"
    );
    // Right button (2) proves the release really keeps the button identity.
    assert_eq!(
        t.encode_mouse_press(2, 10, 5, 0).as_deref(),
        Some(b"\x1b[<2;11;6M".as_slice())
    );
    assert_eq!(
        t.encode_mouse_release(2, 10, 5, 0).as_deref(),
        Some(b"\x1b[<2;11;6m".as_slice()),
        "SGR release of button 2 reports 2, not the X10 release code 3"
    );
}

/// DECSET 1015 (urxvt): `CSI Cb ; Px ; Py M`, Cb = button + 32 and 1-based
/// coordinates, all decimal.
#[test]
fn mouse_1015_urxvt_press_form() {
    let t = term_24x80(b"\x1b[?1000h\x1b[?1015h");
    assert_eq!(
        t.encode_mouse_press(0, 10, 5, 0).as_deref(),
        Some(b"\x1b[32;11;6M".as_slice())
    );
}

/// DECSET 1005 keeps the X10 frame but sends coordinate values 96..2015 as
/// two-byte UTF-8: 0-based col 200 is 201 + 32 = 233 = U+00E9 (0xC3 0xA9).
#[test]
fn mouse_1005_utf8_two_byte_coordinate_beyond_95() {
    let t = term_24x80(b"\x1b[?1000h\x1b[?1005h");
    assert_eq!(
        t.encode_mouse_press(0, 200, 5, 0),
        Some(vec![0x1b, b'[', b'M', 32, 0xC3, 0xA9, 38]),
        "col 233 (after +1+32) is the 2-byte UTF-8 of U+00E9"
    );
}

/// In the X10 encoding a coordinate byte is 32 + the 1-based position, so 223
/// is the largest (xterm's MOUSE_LIMIT). An app that set only DECSET 1000 reads
/// six-byte `ESC [ M Cb Cx Cy` records, so a farther coordinate clamps to byte
/// 255 rather than answering in a form the app never enabled; far columns need
/// 1006 (unlimited) or 1005 (to 2015).
#[test]
fn mouse_x10_coordinate_beyond_223_clamps_and_keeps_the_x10_frame() {
    let t = term_24x80(b"\x1b[?1000h"); // default X10-style encoding
    // 0-based col 300 -> 1-based 301: unrepresentable, so clamped to byte 255.
    assert_eq!(
        t.encode_mouse_press(0, 300, 5, 0),
        Some(vec![0x1b, b'[', b'M', 32, 255, 38]),
        "out-of-range coordinates clamp INSIDE the X10 frame"
    );
    assert_ne!(
        t.encode_mouse_press(0, 300, 5, 0).as_deref(),
        Some(b"\x1b[<0;301;6M".as_slice()),
        "the pre-fix SGR promotion is exactly the defect: a 1006 report the \
         application never requested"
    );
    // Release keeps the frame too, with the legacy button-3 substitution the
    // X10 form requires (no button identity; 1006 provides that).
    assert_eq!(
        t.encode_mouse_release(0, 300, 5, 0),
        Some(vec![0x1b, b'[', b'M', 32 + 3, 255, 38])
    );
    // Boundary: 0-based col 222 -> 1-based 223 is the last single-byte
    // position (32+223 = 255) and is byte-identical to the clamped form.
    assert_eq!(
        t.encode_mouse_press(0, 222, 5, 0),
        Some(vec![0x1b, b'[', b'M', 32, 255, 38]),
        "1-based 223 encodes as byte 255 — the X10 maximum"
    );
    // Negative control: an app that does enable 1006 gets the true coordinate.
    let sgr = term_24x80(b"\x1b[?1000h\x1b[?1006h");
    assert_eq!(
        sgr.encode_mouse_press(0, 300, 5, 0).as_deref(),
        Some(b"\x1b[<0;301;6M".as_slice()),
        "SGR is available on request — it is only the silent promotion that is wrong"
    );
}

/// DECSET 1002 reports motion only while a button is held, with 32 added to
/// the code (left drag: Cb byte 64); buttonless motion (code 3) reports nothing.
#[test]
fn mouse_1002_button_event_motion_requires_held_button() {
    let t = term_24x80(b"\x1b[?1002h");
    assert_eq!(
        t.encode_mouse_motion(0, 10, 5, 0),
        Some(vec![0x1b, b'[', b'M', 64, 43, 38]),
        "drag with left button: Cb = 32 + (0 + 32 motion flag)"
    );
    assert_eq!(
        t.encode_mouse_motion(3, 10, 5, 0),
        None,
        "1002 reports no motion when no button is held"
    );
}

/// DECSET 1003 reports all motion; buttonless motion is code 3 + the motion
/// offset 32 (Cb byte 67).
#[test]
fn mouse_1003_any_event_motion_without_button_uses_code_3() {
    let t = term_24x80(b"\x1b[?1003h");
    assert_eq!(
        t.encode_mouse_motion(3, 10, 5, 0),
        Some(vec![0x1b, b'[', b'M', 67, 43, 38]),
        "buttonless motion: Cb = 32 + (3 + 32)"
    );
}

/// DECSET 1000 reports press and release only, never motion.
#[test]
fn mouse_1000_normal_tracking_reports_no_motion() {
    let t = term_24x80(b"\x1b[?1000h");
    assert_eq!(t.encode_mouse_motion(0, 10, 5, 0), None);
    assert_eq!(t.encode_mouse_motion(3, 10, 5, 0), None);
}

/// With no tracking mode set, or after DECRST 1000, nothing is reported.
#[test]
fn mouse_no_tracking_mode_all_encoders_return_none() {
    let t = term_24x80(b"");
    assert_eq!(t.encode_mouse_press(0, 10, 5, 0), None);
    assert_eq!(t.encode_mouse_release(0, 10, 5, 0), None);
    assert_eq!(t.encode_mouse_motion(0, 10, 5, 0), None);
    assert_eq!(t.encode_mouse_wheel(WheelDir::Up, 10, 5, 0), None);

    let t = term_24x80(b"\x1b[?1000h\x1b[?1000l");
    assert_eq!(t.encode_mouse_press(0, 10, 5, 0), None, "after DECRST 1000");
    assert_eq!(t.encode_mouse_wheel(WheelDir::Up, 10, 5, 0), None);
}

/// DECSET 9 (X10 compatibility) reports presses only.
#[test]
fn mouse_mode_9_x10_compat_is_press_only() {
    let t = term_24x80(b"\x1b[?9h");
    assert_eq!(
        t.encode_mouse_press(0, 10, 5, 0),
        Some(vec![0x1b, b'[', b'M', 32, 43, 38])
    );
    assert_eq!(t.encode_mouse_release(0, 10, 5, 0), None);
    assert_eq!(t.encode_mouse_motion(0, 10, 5, 0), None);
    assert_eq!(t.encode_mouse_wheel(WheelDir::Up, 10, 5, 0), None);
}
