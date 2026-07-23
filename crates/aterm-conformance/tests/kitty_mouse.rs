// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates
//
// Kitty keyboard protocol + mouse tracking encodings — conformance-level
// locks tying DECSET/CSI bytes fed through the VT engine to the exact
// bytes the engine encodes back toward the application.
//
// Specs cited inline per test:
// - Kitty keyboard protocol: https://sw.kovidgoyal.net/kitty/keyboard-protocol/
//   (sections: Progressive enhancement, Disambiguate escape codes,
//    Report all keys as escape codes, modifier encoding)
// - xterm ctlseqs "Mouse Tracking" (X10/normal/button-event/any-event
//   tracking; 1005 UTF-8, 1006 SGR, 1015 urxvt extensions)
//
// The Screen harness has no keyboard/mouse encoder accessors, so these
// tests drive `aterm_core::terminal::Terminal` directly where needed
// (same pattern as vt_categories3.rs::term_24x80).

use aterm_conformance::{Screen, run};
use aterm_core::terminal::Terminal;
use aterm_types::keyboard::{Key, KeyboardMode, Modifiers, NamedKey, encode_key};

/// Feed bytes to a raw 24x80 engine `Terminal` (Screen lacks encoder access).
fn term_24x80(input: &[u8]) -> Terminal {
    let mut t = Terminal::new(24, 80);
    t.process(input);
    t
}

// =========================================================================
// 1. Kitty keyboard: query default flags
// =========================================================================

/// Kitty keyboard protocol, "Detection of support": `CSI ? u` queries the
/// current enhancement flags; the terminal replies `CSI ? flags u`. With no
/// enhancements ever requested, flags must be 0 (all progressive
/// enhancements are opt-in; the power-on default is the legacy protocol).
#[test]
fn kitty_query_default_reports_zero_flags() {
    assert_eq!(run(b"\x1b[?u").response_string(), "\x1b[?0u");
}

// =========================================================================
// 2. Kitty keyboard: push / push / pop / pop round trip
// =========================================================================

/// Kitty keyboard protocol, "Progressive enhancement": `CSI > flags u`
/// pushes onto the per-screen stack and makes `flags` current; `CSI < Ps u`
/// pops Ps entries (default 1), restoring the previous flag set. Flag bits:
/// 0b1 = disambiguate escape codes, 0b100 = report alternate keys, so
/// 5 = disambiguate + alternates. Each step is observed via the `CSI ? u`
/// query reply `CSI ? flags u`.
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

// =========================================================================
// 3. Kitty keyboard: pop on empty stack is clamped
// =========================================================================

/// Kitty keyboard protocol, "Progressive enhancement": "If a pop request is
/// received that empties the stack, all flags are reset" and terminals must
/// tolerate pops larger than the stack depth — there is no underflow error.
/// Popping a fresh (empty-stack) terminal must be a no-op that leaves the
/// flags at 0, and a giant pop count must behave the same.
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

// =========================================================================
// 4. Kitty keyboard: pushed flags change key encoding (DECSET bytes -> keys)
// =========================================================================

/// Kitty keyboard protocol, "Disambiguate escape codes" (flag 0b1): "The Esc
/// key must be reported using CSI u encoding": `CSI 27 u` (27 is the Kitty
/// functional code for Esc). Ctrl+key no longer emits C0 controls: Ctrl+C is
/// `CSI 99 ; 5 u` (codepoint 99 = 'c'; modifier field = 1 + ctrl(4) = 5 per
/// the spec's modifier encoding). This is the conformance lock tying the
/// fed `CSI > 1 u` bytes to Terminal::keyboard_mode() + encode_key output.
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

/// Kitty keyboard protocol, "Disambiguate escape codes": keys that already
/// have non-text legacy encodings (arrows, Home/End, F-keys...) "continue to
/// be reported using their legacy encoding" unless flag 0b1000 (report all
/// keys as escape codes) is also set. Plain ArrowUp must stay `CSI A`.
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

/// Kitty keyboard protocol, "Report all keys as escape codes" (flag 0b1000):
/// "all keys, including those that produce text, are reported as escape
/// codes" — a plain 'a' press becomes `CSI 97 u` instead of the byte 0x61.
/// Set via `CSI = 8 ; 1 u` (mode 1 = set exactly these bits).
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

/// Kitty keyboard protocol: popping the enhancement off the stack restores
/// the prior (here: legacy) encoding — the lifecycle is fully reversible.
/// After `CSI > 1 u` then `CSI < u`, Esc must encode as plain 0x1B again.
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

// =========================================================================
// 5. Mouse: mode 1000, default (X10-style) byte encoding
// =========================================================================

/// xterm ctlseqs, Mouse Tracking, "Normal tracking mode" (DECSET 1000):
/// press/release are reported as `CSI M Cb Cx Cy` where Cb = button + 32
/// (left=0, middle=1, right=2; release uses 3) and Cx/Cy are the 1-based
/// position + 32. 0-based (col 10, row 5) is 1-based (11, 6), so
/// Cx = 32+11 = 43, Cy = 32+6 = 38.
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

/// xterm ctlseqs, Mouse Tracking: "Wheel mice may return buttons 4 and 5
/// ... the event codes are 64 and 65" — wheel-up is button code 64, so the
/// Cb byte is 32 + 64 = 96. Modifiers add: shift=4 (lock one combination).
#[test]
fn mouse_1000_wheel_up_is_button_64_and_shift_adds_4() {
    let t = term_24x80(b"\x1b[?1000h");
    assert_eq!(
        t.encode_mouse_wheel(true, 10, 5, 0),
        Some(vec![0x1b, b'[', b'M', 96, 43, 38]),
        "wheel up: Cb = 32 + 64"
    );
    // ctlseqs: shift adds 4 to the button code in tracking modes.
    assert_eq!(
        t.encode_mouse_press(0, 10, 5, 4),
        Some(vec![0x1b, b'[', b'M', 36, 43, 38]),
        "shift+left press: Cb = 32 + (0|4)"
    );
}

// =========================================================================
// 6. Mouse: SGR extended encoding (DECSET 1006)
// =========================================================================

/// xterm ctlseqs, "SGR (1006)" extended mouse mode: events are reported as
/// `CSI < Cb ; Px ; Py M` for press and the same with final `m` for release.
/// Unlike X10, the button code is NOT offset by 32 and the release event
/// carries the ORIGINAL button number (the final character disambiguates),
/// so applications can tell which button was released.
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

// =========================================================================
// 7. Mouse: urxvt extended encoding (DECSET 1015)
// =========================================================================

/// xterm ctlseqs, "URXVT (1015)" extended mouse mode: events are reported
/// as `CSI Cb ; Px ; Py M` where Cb is the X10-style code (button + 32) in
/// DECIMAL and the coordinates are 1-based decimal — left press at 0-based
/// (10, 5) is `CSI 32 ; 11 ; 6 M`.
#[test]
fn mouse_1015_urxvt_press_form() {
    let t = term_24x80(b"\x1b[?1000h\x1b[?1015h");
    assert_eq!(
        t.encode_mouse_press(0, 10, 5, 0).as_deref(),
        Some(b"\x1b[32;11;6M".as_slice())
    );
}

// =========================================================================
// 8. Mouse: UTF-8 extended encoding (DECSET 1005), coords > 95
// =========================================================================

/// xterm ctlseqs, "UTF-8 (1005)" extended mouse mode: the format matches
/// X10 (`CSI M Cb Cx Cy`) but coordinate values 96..2015 (offset value
/// 128..2047) are emitted as 2-byte UTF-8. 0-based col 200 -> 1-based 201
/// -> 201+32 = 233 = U+00E9 -> bytes 0xC3 0xA9; row 5 -> 6+32 = 38 stays a
/// single byte.
#[test]
fn mouse_1005_utf8_two_byte_coordinate_beyond_95() {
    let t = term_24x80(b"\x1b[?1000h\x1b[?1005h");
    assert_eq!(
        t.encode_mouse_press(0, 200, 5, 0),
        Some(vec![0x1b, b'[', b'M', 32, 0xC3, 0xA9, 38]),
        "col 233 (after +1+32) is the 2-byte UTF-8 of U+00E9"
    );
}

// =========================================================================
// 9. Mouse: X10-style coordinates beyond 223
// =========================================================================

/// xterm ctlseqs, Mouse Tracking: in the default (non-extended) encoding a
/// coordinate byte is `Cx = 32 + x` with x 1-based, so the maximum
/// representable position is 223 (32+223 = 255); larger positions "cannot
/// be reported" in a single byte (xterm's MOUSE_LIMIT). The byte encoding
/// simply has no representation for them, so the engine's DOCUMENTED choice
/// (aterm_types::mouse::encode_mouse, matching foot) is to fall back to the
/// SGR form, which is unambiguous and well-formed for any coordinate. This
/// test locks that fallback: sane bytes, no overflow, no truncated/malformed
/// single-byte garbage (the historical xterm wraparound bug).
#[test]
fn mouse_x10_coordinate_beyond_223_falls_back_to_sgr() {
    let t = term_24x80(b"\x1b[?1000h"); // default X10-style encoding
    // 0-based col 300 -> 1-based 301: unrepresentable in one byte.
    assert_eq!(
        t.encode_mouse_press(0, 300, 5, 0).as_deref(),
        Some(b"\x1b[<0;301;6M".as_slice()),
        "engine-documented fallback: SGR form for out-of-range coordinates"
    );
    // Release through the same fallback is a TRUE SGR release: 'm' terminator
    // AND the button identity preserved (SGR semantics — only legacy single-
    // byte forms substitute button 3; the encoder decides per actual output
    // format, #7473).
    assert_eq!(
        t.encode_mouse_release(0, 300, 5, 0).as_deref(),
        Some(b"\x1b[<0;301;6m".as_slice())
    );
    // Boundary: 0-based col 222 -> 1-based 223 is the LAST single-byte
    // position (32+223 = 255) and must still use the X10 byte form.
    assert_eq!(
        t.encode_mouse_press(0, 222, 5, 0),
        Some(vec![0x1b, b'[', b'M', 32, 255, 38]),
        "1-based 223 encodes as byte 255 — the X10 maximum"
    );
}

// =========================================================================
// 10. Mouse: motion gating under 1002 (button-event) and 1003 (any-event)
// =========================================================================

/// xterm ctlseqs, "Button-event tracking" (DECSET 1002): motion is reported
/// ONLY while a button is down, with 32 added to the button code (left drag
/// Cb = 0+32 -> byte 32+32 = 64). Motion with no button held (X10 "button"
/// 3) must report nothing under 1002.
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

/// xterm ctlseqs, "Any-event tracking" (DECSET 1003): "all motion events
/// are reported" even with no button down; a buttonless motion uses the
/// release/no-button code 3 plus the motion offset 32, i.e. Cb = 35 ->
/// byte 32+35 = 67.
#[test]
fn mouse_1003_any_event_motion_without_button_uses_code_3() {
    let t = term_24x80(b"\x1b[?1003h");
    assert_eq!(
        t.encode_mouse_motion(3, 10, 5, 0),
        Some(vec![0x1b, b'[', b'M', 67, 43, 38]),
        "buttonless motion: Cb = 32 + (3 + 32)"
    );
}

/// xterm ctlseqs, "Normal tracking mode" (DECSET 1000) reports only press
/// and release — no motion events at all, held button or not.
#[test]
fn mouse_1000_normal_tracking_reports_no_motion() {
    let t = term_24x80(b"\x1b[?1000h");
    assert_eq!(t.encode_mouse_motion(0, 10, 5, 0), None);
    assert_eq!(t.encode_mouse_motion(3, 10, 5, 0), None);
}

// =========================================================================
// 11. Mouse: no tracking mode -> no reports
// =========================================================================

/// xterm ctlseqs, Mouse Tracking: mouse reporting is opt-in via DECSET;
/// with no tracking mode set the terminal sends NO mouse sequences. Also
/// holds after enabling and then disabling tracking (DECRST 1000).
#[test]
fn mouse_no_tracking_mode_all_encoders_return_none() {
    let t = term_24x80(b"");
    assert_eq!(t.encode_mouse_press(0, 10, 5, 0), None);
    assert_eq!(t.encode_mouse_release(0, 10, 5, 0), None);
    assert_eq!(t.encode_mouse_motion(0, 10, 5, 0), None);
    assert_eq!(t.encode_mouse_wheel(true, 10, 5, 0), None);

    let t = term_24x80(b"\x1b[?1000h\x1b[?1000l");
    assert_eq!(t.encode_mouse_press(0, 10, 5, 0), None, "after DECRST 1000");
    assert_eq!(t.encode_mouse_wheel(true, 10, 5, 0), None);
}

/// xterm ctlseqs, "X10 compatibility mode" (DECSET 9): reports button
/// PRESS only — no release, motion, or wheel events.
#[test]
fn mouse_mode_9_x10_compat_is_press_only() {
    let t = term_24x80(b"\x1b[?9h");
    assert_eq!(
        t.encode_mouse_press(0, 10, 5, 0),
        Some(vec![0x1b, b'[', b'M', 32, 43, 38])
    );
    assert_eq!(t.encode_mouse_release(0, 10, 5, 0), None);
    assert_eq!(t.encode_mouse_motion(0, 10, 5, 0), None);
    assert_eq!(t.encode_mouse_wheel(true, 10, 5, 0), None);
}
