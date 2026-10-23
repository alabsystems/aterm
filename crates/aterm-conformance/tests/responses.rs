// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use aterm_conformance::{Screen, run};

#[test]
fn responses_accumulate_until_drained_then_buffer_is_empty() {
    let mut s = run(b"\x1b[5n\x1b[6n");
    assert_eq!(s.response_string(), "\x1b[0n\x1b[1;1R");
    assert_eq!(s.take_response(), None);
}

#[test]
fn every_query_gets_its_exact_reply() {
    let cases: &[(&str, &[u8], &str)] = &[
        ("DA1", b"\x1b[c", "\x1b[?64;6;22;28;4c"),
        ("DA1 with Ps=0", b"\x1b[0c", "\x1b[?64;6;22;28;4c"),
        ("DA2", b"\x1b[>c", "\x1b[>41;100;0c"),
        ("DA3", b"\x1b[=c", "\x1bP!|30\x1b\\"),
        ("DSR 5", b"\x1b[5n", "\x1b[0n"),
        ("CPR at home", b"\x1b[6n", "\x1b[1;1R"),
        ("CPR after CUP", b"\x1b[10;20H\x1b[6n", "\x1b[10;20R"),
        (
            "CPR under DECOM",
            b"\x1b[5;20r\x1b[?6h\x1b[1;1H\x1b[6n",
            "\x1b[1;1R",
        ),
        ("DECXCPR at home", b"\x1b[?6n", "\x1b[?1;1;1R"),
        (
            "DECXCPR after CUP",
            b"\x1b[10;20H\x1b[?6n",
            "\x1b[?10;20;1R",
        ),
        ("DECRQM ?25", b"\x1b[?25$p", "\x1b[?25;1$y"),
        ("DECRQM ?6", b"\x1b[?6$p", "\x1b[?6;2$y"),
        ("DECRQM ?2048", b"\x1b[?2048$p", "\x1b[?2048;2$y"),
        (
            "DECRQM ?2048 after kitty push",
            b"\x1b[>1u\x1b[?2048$p",
            "\x1b[?2048;2$y",
        ),
        ("kitty flags query", b"\x1b[>1u\x1b[?u", "\x1b[?1u"),
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
    let mut s = Screen::new(24, 80);
    s.feed(b"\x1bP$qzz\x1b\\ok");
    assert_eq!(s.response_string(), "\x1bP0$r\x1b\\");
    assert_eq!(s.row(0), "ok");
}

#[test]
fn plain_text_produces_no_response() {
    assert_eq!(run(b"hello").take_response(), None);
}
