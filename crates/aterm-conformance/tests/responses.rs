// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates
//
// Response correctness — feed a query, drain Terminal::take_response(), assert
// the EXACT reply bytes. Expectations are xterm/VT510-correct; where the reply
// embeds an identity constant (DA1/DA2/DA3 device params, firmware version),
// the engine's own documented identity is asserted and marked as such.

use aterm_conformance::{Screen, run};

/// Feed `input` into a fresh 24x80 screen and drain the reply as a string.
fn resp(input: &[u8]) -> String {
    run(input).response_string()
}

/// Feed `input` and drain the raw reply bytes (None if no reply pending).
fn resp_bytes(input: &[u8]) -> Option<Vec<u8>> {
    run(input).take_response()
}

// --- drain semantics -------------------------------------------------------

#[test]
fn responses_accumulate_until_drained_then_buffer_is_empty() {
    // Engine contract: feed()/process() never auto-drain; replies concatenate
    // in arrival order until take_response() drains them (buffer_api.rs).
    let mut s = run(b"\x1b[5n\x1b[6n");
    assert_eq!(s.response_string(), "\x1b[0n\x1b[1;1R");
    // Drained: a second take must report nothing pending.
    assert_eq!(s.take_response(), None);
}

// --- device attributes -----------------------------------------------------

#[test]
fn da1_reports_vt420_with_capabilities() {
    // VT510 DA1 format: CSI ? Pc ; Ps.. c. 64/6/22/28 (VT420 level, selective
    // erase, ANSI color, rectangular editing) plus 4 (sixel) — the engine's
    // documented identity constant (handler_report.rs). Code 4 is advertised
    // because aterm-conformance builds aterm-core with the `sixel` feature on,
    // so the live decode/render path exists.
    assert_eq!(resp(b"\x1b[c"), "\x1b[?64;6;22;28;4c");
}

#[test]
fn da1_with_explicit_zero_param_is_same_as_omitted() {
    // xterm: CSI 0 c is equivalent to CSI c (Ps = 0 or omitted requests DA1).
    assert_eq!(resp(b"\x1b[0c"), "\x1b[?64;6;22;28;4c");
}

#[test]
fn da2_reports_type_version_cartridge() {
    // xterm DA2 format: CSI > Pp ; Pv ; Pc c. 41 (VT420) / 100 (firmware
    // 1.0.0) / 0 (no ROM cartridge) are engine identity constants.
    assert_eq!(resp(b"\x1b[>c"), "\x1b[>41;100;0c");
}

#[test]
fn da3_reports_unit_id_as_dcs_string() {
    // VT510 DA3 format: DCS ! | D..D ST (hex-encoded unit ID). The payload
    // "30" (hex for ASCII '0' = no unit ID) is the engine's documented
    // identity constant; xterm uses "00000000".
    assert_eq!(resp(b"\x1b[=c"), "\x1bP!|30\x1b\\");
}

// --- device status reports -------------------------------------------------

#[test]
fn dsr5_reports_terminal_ok() {
    // VT510 DSR: CSI 5 n requests operating status; "ready" is CSI 0 n.
    assert_eq!(resp(b"\x1b[5n"), "\x1b[0n");
}

#[test]
fn cpr_at_home_reports_1_1() {
    // VT510 CPR: CSI 6 n -> CSI Pr ; Pc R, 1-indexed; home is 1;1.
    assert_eq!(resp(b"\x1b[6n"), "\x1b[1;1R");
}

#[test]
fn cpr_after_cup_reports_10_20() {
    // VT510 CPR reports the active position set by CUP (both 1-indexed).
    assert_eq!(resp(b"\x1b[10;20H\x1b[6n"), "\x1b[10;20R");
}

#[test]
fn cpr_with_origin_mode_is_margin_relative() {
    // VT510: with DECOM set, CPR reports the row relative to the top margin,
    // so DECSTBM 5..20 + CUP 1;1 (= absolute row 5) must report 1;1, not 5;1.
    assert_eq!(resp(b"\x1b[5;20r\x1b[?6h\x1b[1;1H\x1b[6n"), "\x1b[1;1R");
}

#[test]
fn decxcpr_reports_row_col_page() {
    // VT510 DECXCPR: CSI ? 6 n -> CSI ? Pr ; Pc ; Pp R with page always 1.
    assert_eq!(resp(b"\x1b[?6n"), "\x1b[?1;1;1R");
}

#[test]
fn decxcpr_after_cup_reports_10_20_page_1() {
    // VT510 DECXCPR tracks the active position like CPR, plus the page number.
    assert_eq!(resp(b"\x1b[10;20H\x1b[?6n"), "\x1b[?10;20;1R");
}

// --- DECRQM ----------------------------------------------------------------

#[test]
fn decrqm_reports_set_for_visible_cursor() {
    // VT510 DECRQM: CSI ? 25 $ p -> CSI ? 25 ; Ps $ y, Ps=1 (set) since
    // DECTCEM (cursor visible) is the power-on default.
    assert_eq!(resp(b"\x1b[?25$p"), "\x1b[?25;1$y");
}

#[test]
fn decrqm_reports_reset_for_origin_mode_off() {
    // VT510 DECRQM: CSI ? 6 $ p -> CSI ? 6 ; 2 $ y, Ps=2 (reset) since
    // DECOM is reset at power-on.
    assert_eq!(resp(b"\x1b[?6$p"), "\x1b[?6;2$y");
}

#[test]
fn decrqm_2048_in_band_resize_reports_reset_when_off() {
    // xterm DECRPM: Ps=2 = mode recognized but reset. In-band resize
    // notifications (mode 2048) are now implemented — aterm emits a size report
    // on enable and on every resize — so DECRQM reports the real state: Ps=2
    // (reset) at power-on since 2048 is off by default, including after the
    // kitty keyboard protocol is enabled. Neovim 0.10+ pushes kitty flags and
    // then probes 2048; because aterm actually delivers the notifications,
    // reporting the mode as known (not the Ps=0 "not recognized") is correct
    // and does not strand the client. See handler_dec.rs decrqm_reports_mode_
    // 2048_state for the on/off state coverage; this pins the kitty-keyboard
    // arm against the historical 2048 misrouting.
    assert_eq!(resp(b"\x1b[?2048$p"), "\x1b[?2048;2$y");
    assert_eq!(resp(b"\x1b[>1u\x1b[?2048$p"), "\x1b[?2048;2$y");
}

#[test]
fn kitty_keyboard_flags_query_uses_csi_question_u_not_decrqm() {
    // Kitty keyboard protocol: flags are reported via CSI ? u -> CSI ? Pf u.
    // The protocol has no DEC private mode number, so DECRQM is never the
    // channel for it (the misrouted 2048 arm this pins against).
    assert_eq!(resp(b"\x1b[>1u\x1b[?u"), "\x1b[?1u");
}

// --- DECRQSS ---------------------------------------------------------------
// Validity code: the engine follows xterm's convention — DCS 1 $ r .. ST for a
// VALID request, DCS 0 $ r ST for invalid. Note this is INVERTED from the DEC
// VT510/VT520 manuals, where 0 means valid and 1 means invalid; xterm's
// convention is what modern applications expect.

#[test]
fn decrqss_sgr_reports_current_attributes() {
    // xterm DECRQSS "m": DCS 1 $ r 0;<SGR params> m ST after SGR 1;31 — the
    // leading default-reset "0;" makes the report a self-contained replayable
    // style (xterm emits "0;1;31", never a bare "1;31").
    assert_eq!(resp(b"\x1b[1;31m\x1bP$qm\x1b\\"), "\x1bP1$r0;1;31m\x1b\\");
}

#[test]
fn decrqss_sgr_default_state_reports_0() {
    // xterm DECRQSS "m" with all attributes default reports SGR 0.
    assert_eq!(resp(b"\x1bP$qm\x1b\\"), "\x1bP1$r0m\x1b\\");
}

#[test]
fn decrqss_decstbm_reports_margins() {
    // VT510 DECRQSS "r": reports DECSTBM top;bottom (1-indexed) -> 3;10.
    assert_eq!(resp(b"\x1b[3;10r\x1bP$qr\x1b\\"), "\x1bP1$r3;10r\x1b\\");
}

#[test]
fn decrqss_decsca_reports_protection_attribute() {
    // VT510 DECRQSS '"q' (DECSCA): 0 = not protected at power-on default.
    assert_eq!(resp(b"\x1bP$q\"q\x1b\\"), "\x1bP1$r0\"q\x1b\\");
    // After CSI 1 " q (protect on), DECSCA must report 1.
    assert_eq!(resp(b"\x1b[1\"q\x1bP$q\"q\x1b\\"), "\x1bP1$r1\"q\x1b\\");
}

#[test]
fn decrqss_decscusr_reports_cursor_style() {
    // xterm DECRQSS " q" (DECSCUSR): default cursor is blinking block = 1.
    assert_eq!(resp(b"\x1bP$q q\x1b\\"), "\x1bP1$r1 q\x1b\\");
    // After CSI 4 SP q (steady underline), DECSCUSR must report 4.
    assert_eq!(resp(b"\x1b[4 q\x1bP$q q\x1b\\"), "\x1bP1$r4 q\x1b\\");
}

#[test]
fn decrqss_decscl_reports_level_and_fixed_7bit_c1() {
    // DECSCL ("p) reports the conformance level plus the C1 *transmission* mode
    // (1 = 8-bit / S8C1T, 2 = 7-bit / S7C1T). aterm has a FIXED 7-bit C1
    // transmission policy: even when DECSCL is SET requesting 8-bit C1 (Pc = 1),
    // the report must come back 7-bit (;2). The set-side deliberately ignores the
    // C1 mode and the report mirrors the real, invariant state — not a placeholder.
    // Set VT520 (Pl = 65) with 8-bit C1 requested, then query DECSCL.
    assert_eq!(
        resp(b"\x1b[65;1\"p\x1bP$q\"p\x1b\\"),
        "\x1bP1$r65;2\"p\x1b\\"
    );
}

#[test]
fn decrqss_unknown_setting_reports_invalid() {
    // xterm: an unrecognized DECRQSS Pt gets the invalid form DCS 0 $ r ST
    // (validity 0 = invalid in xterm's convention) — and must not panic.
    assert_eq!(resp(b"\x1bP$qzz\x1b\\"), "\x1bP0$r\x1b\\");
}

#[test]
fn decrqss_unknown_leaves_terminal_usable() {
    // After an invalid DECRQSS the terminal must keep parsing normally.
    let mut s = Screen::new(24, 80);
    s.feed(b"\x1bP$qzz\x1b\\ok");
    assert_eq!(s.response_string(), "\x1bP0$r\x1b\\");
    assert_eq!(s.row(0), "ok");
}

/// No-response queries must leave the buffer empty (take_response -> None).
#[test]
fn plain_text_produces_no_response() {
    assert_eq!(resp_bytes(b"hello"), None);
}
