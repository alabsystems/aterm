// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates
//
// Responses: feed a query, drain `Terminal::take_response()`, assert the exact
// reply bytes. Expectations are xterm/VT510-correct; where a reply embeds an
// identity constant (DA1/DA2/DA3 device params, firmware version), the
// engine's own identity is asserted and marked as such.

use aterm_conformance::{Screen, run};

#[test]
fn responses_accumulate_until_drained_then_buffer_is_empty() {
    // feed() never drains: replies concatenate in arrival order until taken.
    let mut s = run(b"\x1b[5n\x1b[6n");
    assert_eq!(s.response_string(), "\x1b[0n\x1b[1;1R");
    assert_eq!(s.take_response(), None);
}

/// Each query's exact reply on a fresh 24x80 screen.
///
/// DECRQSS validity: the engine follows xterm's convention — DCS 1 $ r .. ST
/// for a valid request, DCS 0 $ r ST for invalid. This is inverted from the
/// DEC VT510/VT520 manuals; xterm's convention is what applications expect.
#[test]
fn every_query_gets_its_exact_reply() {
    let cases: &[(&str, &[u8], &str)] = &[
        // VT510 DA1: CSI ? Pc ; Ps.. c. 64/6/22/28 (VT420 level, selective
        // erase, ANSI color, rectangular editing) plus 4 (sixel): identity constant.
        ("DA1", b"\x1b[c", "\x1b[?64;6;22;28;4c"),
        // xterm: CSI 0 c is equivalent to CSI c.
        ("DA1 with Ps=0", b"\x1b[0c", "\x1b[?64;6;22;28;4c"),
        // xterm DA2: CSI > Pp ; Pv ; Pc c — VT420 / firmware 1.0.0 / no ROM.
        ("DA2", b"\x1b[>c", "\x1b[>41;100;0c"),
        // VT510 DA3: DCS ! | D..D ST; "30" (ASCII '0' in hex) = no unit ID.
        ("DA3", b"\x1b[=c", "\x1bP!|30\x1b\\"),
        // VT510 DSR: CSI 5 n -> CSI 0 n (ready).
        ("DSR 5", b"\x1b[5n", "\x1b[0n"),
        // VT510 CPR: CSI 6 n -> CSI Pr ; Pc R, 1-indexed.
        ("CPR at home", b"\x1b[6n", "\x1b[1;1R"),
        ("CPR after CUP", b"\x1b[10;20H\x1b[6n", "\x1b[10;20R"),
        // With DECOM set, CPR is relative to the top margin (absolute row 5).
        (
            "CPR under DECOM",
            b"\x1b[5;20r\x1b[?6h\x1b[1;1H\x1b[6n",
            "\x1b[1;1R",
        ),
        // VT510 DECXCPR: CSI ? 6 n -> CSI ? Pr ; Pc ; Pp R, page always 1.
        ("DECXCPR at home", b"\x1b[?6n", "\x1b[?1;1;1R"),
        (
            "DECXCPR after CUP",
            b"\x1b[10;20H\x1b[?6n",
            "\x1b[?10;20;1R",
        ),
        // VT510 DECRQM: Ps=1 set (DECTCEM is on at power-on), Ps=2 reset.
        ("DECRQM ?25", b"\x1b[?25$p", "\x1b[?25;1$y"),
        ("DECRQM ?6", b"\x1b[?6$p", "\x1b[?6;2$y"),
        // In-band resize (2048) is implemented and reset at power-on, also
        // after the kitty keyboard protocol is pushed (Neovim 0.10+ probes it).
        ("DECRQM ?2048", b"\x1b[?2048$p", "\x1b[?2048;2$y"),
        (
            "DECRQM ?2048 after kitty push",
            b"\x1b[>1u\x1b[?2048$p",
            "\x1b[?2048;2$y",
        ),
        // Kitty keyboard flags answer CSI ? u, never DECRQM.
        ("kitty flags query", b"\x1b[>1u\x1b[?u", "\x1b[?1u"),
        // xterm DECRQSS "m": the leading "0;" makes the report replayable.
        (
            "DECRQSS SGR",
            b"\x1b[1;31m\x1bP$qm\x1b\\",
            "\x1bP1$r0;1;31m\x1b\\",
        ),
        ("DECRQSS SGR default", b"\x1bP$qm\x1b\\", "\x1bP1$r0m\x1b\\"),
        (
            "DECRQSS DECSTBM",
            b"\x1b[3;10r\x1bP$qr\x1b\\",
            "\x1bP1$r3;10r\x1b\\",
        ),
        (
            "DECRQSS DECSCA off",
            b"\x1bP$q\"q\x1b\\",
            "\x1bP1$r0\"q\x1b\\",
        ),
        (
            "DECRQSS DECSCA on",
            b"\x1b[1\"q\x1bP$q\"q\x1b\\",
            "\x1bP1$r1\"q\x1b\\",
        ),
        // Default cursor is the blinking block (1); after CSI 4 SP q, 4.
        (
            "DECRQSS DECSCUSR default",
            b"\x1bP$q q\x1b\\",
            "\x1bP1$r1 q\x1b\\",
        ),
        (
            "DECRQSS DECSCUSR set",
            b"\x1b[4 q\x1bP$q q\x1b\\",
            "\x1bP1$r4 q\x1b\\",
        ),
        // DECSCL: level plus C1 transmission (1 = 7-bit, 0 or 2 = 8-bit);
        // aterm's C1 is fixed 7-bit (1) even when 8-bit (Pc = 0) is requested.
        (
            "DECRQSS DECSCL",
            b"\x1b[65;0\"p\x1bP$q\"p\x1b\\",
            "\x1bP1$r65;1\"p\x1b\\",
        ),
    ];
    for (name, query, reply) in cases {
        assert_eq!(run(query).response_string(), *reply, "{name}");
    }
}

#[test]
fn decrqss_unknown_reports_invalid_and_leaves_terminal_usable() {
    // xterm: an unrecognized DECRQSS Pt gets DCS 0 $ r ST, and parsing goes on.
    let mut s = Screen::new(24, 80);
    s.feed(b"\x1bP$qzz\x1b\\ok");
    assert_eq!(s.response_string(), "\x1bP0$r\x1b\\");
    assert_eq!(s.row(0), "ok");
}

#[test]
fn plain_text_produces_no_response() {
    assert_eq!(run(b"hello").take_response(), None);
}
