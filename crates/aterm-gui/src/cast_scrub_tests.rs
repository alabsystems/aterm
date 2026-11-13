// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use super::*;

fn scrub(slices: &[&[u8]]) -> Vec<u8> {
    let mut scrub = CastScrub::starting(true);
    let mut out = Vec::new();
    for slice in slices {
        scrub.feed(slice, &mut out);
    }
    scrub.finish(&mut out);
    out
}

#[test]
fn a_shell_mark_keeps_its_kind_and_loses_its_nonce_even_split_across_slices() {
    let nonce = "3c".repeat(32);
    let marks = format!(
        "\x1b]133;A;id={nonce}\x07$ \x1b]133;B;id={nonce};aterm_input=start\x1b\\ls\r\n\
         \x1b]133;D;0;id={nonce}\x07\x1b]633;E;ls -la;id={nonce}\x07\x1b]633;P;Cwd=/tmp;id={nonce}\x07"
    );
    let bytes = marks.as_bytes();
    for cut in 0..bytes.len() {
        let out = scrub(&[&bytes[..cut], &bytes[cut..]]);
        let text = String::from_utf8(out).expect("utf-8");
        assert!(!text.contains(&nonce), "cut at {cut}: {text:?}");
        assert_eq!(
            text,
            "\x1b]133;A\x07$ \x1b]133;B;aterm_input=start\x1b\\ls\r\n\x1b]133;D;0\x07\
             \x1b]633;E;ls -la\x07\x1b]633;P;Cwd=/tmp\x07",
            "cut at {cut}"
        );
    }
}

#[test]
fn invisible_payloads_and_queries_go_and_what_paints_stays() {
    let out = scrub(&[
        b"a\x1b]52;c;c2VjcmV0\x07b",
        b"\x1b]1337;SetUserVar=k=dg==\x07c\x1bPtmux;\x1b\x1b]52;c;eA==\x07\x1b\\d",
        b"\x1bP$qm\x1b\\e\x1b]9;hi\x07\x1b]10;?\x07\x1b]4;1;?\x07\x1b_Ga=q,i=1;AAAA\x1b\\",
        b"\x1b]0;title\x07\x1b]8;;https://x\x1b\\link\x1b]8;;\x1b\\\x1b[31mred\x1b[m\x1b(0q\x1b(B",
    ]);
    assert_eq!(
        out,
        b"abcde\x1b]0;title\x07\x1b]8;;https://x\x1b\\link\x1b]8;;\x1b\\\x1b[31mred\x1b[m\x1b(0q\x1b(B"
    );
}

#[test]
fn an_image_that_carries_its_pixels_passes_and_one_naming_a_file_goes() {
    let sixel = b"\x1bP0;1q\"1;1;2;2#0;2;0;0;0#0~~\x1b\\";
    let kitty = b"\x1b_Ga=T,f=100;iVBORw0KGgo=\x1b\\";
    let iterm = b"\x1b]1337;File=name=eA==;inline=1:R0lGODlh\x07";
    let mut shown = Vec::new();
    for image in [&sixel[..], kitty, iterm] {
        shown.extend_from_slice(image);
        shown.push(b'.');
    }
    for cut in 0..shown.len() {
        assert_eq!(
            scrub(&[&shown[..cut], &shown[cut..]]),
            shown,
            "cut at {cut}"
        );
    }
    let hidden: &[&[u8]] = &[
        b"\x1b_Ga=T,t=f;L2V0Yy9wYXNzd2Q=\x1b\\",
        b"\x1b_Gt=s,a=T;c2htLW5hbWU=\x1b\\",
        b"\x1b]1337;File=name=eA==;size=3:c2VjcmV0\x07",
        b"\x1bP1$r0m\x1b\\",
        b"\x1bP+q544e\x1b\\",
    ];
    for bytes in hidden {
        assert_eq!(scrub(&[bytes, b"."]), b".", "{bytes:?}");
    }
}

#[test]
fn an_unknown_start_drops_a_strings_tail_and_lets_text_out() {
    let tail = |bytes: &[u8]| {
        let mut scrub = CastScrub::starting(false);
        let mut out = Vec::new();
        scrub.feed(bytes, &mut out);
        scrub.finish(&mut out);
        out
    };
    let nonce = "ab".repeat(32);
    assert_eq!(tail(format!(";id={nonce}\x07$ ").as_bytes()), b"$ ");
    assert_eq!(tail(format!(";id={nonce}\x1b\\$ ").as_bytes()), b"$ ");
    assert_eq!(tail(b"plain text\x1b[1mbold"), b"plain text\x1b[1mbold");
    assert_eq!(tail(b"no escape at all"), b"no escape at all");
}

#[test]
fn a_restart_drops_a_string_left_open() {
    let mut scrub = CastScrub::starting(true);
    let mut out = Vec::new();
    scrub.feed(b"x\x1b]52;c;c2Vj", &mut out);
    scrub.restart(true);
    scrub.feed(b"y\x1b]2;t\x07", &mut out);
    assert_eq!(out, b"xy\x1b]2;t\x07");
}
