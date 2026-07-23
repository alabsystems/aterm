// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0
//
// Author: Andrew Yates
//
//! Integration tests for `OutputSanitizer` — the Containment-mode output
//! filter (#7901). Feeds adversarial byte streams through the sanitizer
//! and asserts that:
//!
//! 1. OSC/DCS/APC/SOS/PM sequences are fully stripped — dangerous escape
//!    bytes must not appear anywhere in the returned output.
//! 2. Benign output (plain text, SGR coloring, cursor movement, UTF-8) is
//!    preserved byte-for-byte.
//! 3. Streaming: sequences split across multiple [`OutputSanitizer::sanitize`]
//!    calls are still fully stripped.

use aterm_containment::OutputSanitizer;

/// Convenience: sanitize a full slice with a fresh sanitizer.
fn sanitize_fresh(input: &[u8]) -> Vec<u8> {
    let mut s = OutputSanitizer::new();
    s.sanitize(input)
}

/// Assert that the sanitized output contains no ESC or BEL bytes — these
/// are the control bytes that would indicate a control sequence leaked
/// through. Useful as a defense-in-depth check for purely-hostile inputs.
///
/// Note: C1 single bytes (0x80-0x9F) are legitimate UTF-8 continuation
/// bytes, so this check does NOT flag them. See module docs in
/// `aterm_containment::output_filter` for the rationale.
fn assert_no_control_escape_bytes(bytes: &[u8]) {
    for (i, &b) in bytes.iter().enumerate() {
        assert!(
            b != 0x1B && b != 0x07,
            "byte {b:#04x} at index {i} leaked through sanitizer in {bytes:?}",
        );
    }
}

// ---- Adversarial streams (MUST be stripped) ----

#[test]
fn clipboard_write_osc52_is_stripped() {
    // Classic OSC 52 clipboard write — would otherwise set the user's
    // clipboard to attacker-controlled content.
    let payload = b"\x1b]52;c;YXR0YWNrZXItY29udGVudA==\x07";
    let out = sanitize_fresh(payload);
    assert_eq!(out, b"");
    assert_no_control_escape_bytes(&out);
}

#[test]
fn title_hijack_osc0_is_stripped() {
    // OSC 0 — icon name + window title.
    let payload = b"\x1b]0;ATTACKER-CHOSEN-TITLE\x07";
    assert_eq!(sanitize_fresh(payload), b"");
}

#[test]
fn title_hijack_osc2_st_terminated() {
    // OSC 2 terminated by String Terminator instead of BEL.
    let payload = b"\x1b]2;ATTACKER\x1b\\";
    assert_eq!(sanitize_fresh(payload), b"");
}

#[test]
fn hyperlink_wrapping_is_stripped_but_text_survives() {
    // OSC 8 hyperlink wrapping: the attacker can redirect a plaintext word
    // to any URL. We strip both endpoints; the inner text remains.
    let payload = b"\x1b]8;;https://evil.example.com/x\x1b\\click me\x1b]8;;\x1b\\";
    assert_eq!(sanitize_fresh(payload), b"click me");
}

#[test]
fn dcs_tmux_passthrough_is_stripped() {
    // tmux DCS pass-through: wraps another OSC in a DCS so it reaches
    // outer terminals. Both layers must vanish.
    let payload = b"before\x1bPtmux;\x1b]0;INNER TITLE\x07\x1b\\after";
    assert_eq!(sanitize_fresh(payload), b"beforeafter");
}

#[test]
fn dcs_sixel_image_is_stripped() {
    // Sixel image data: DCS ... ST. Arbitrary payload in between.
    let payload = b"before\x1bPq#0;2;100;100;100#0!14~\x1b\\after";
    assert_eq!(sanitize_fresh(payload), b"beforeafter");
}

#[test]
fn apc_payload_is_stripped() {
    // APC — various terminal AI / image features use APC today. For the
    // Containment tier we deny the entire surface.
    let payload = b"a\x1b_Gq=1,f=100,a=T,t=d,s=100,v=100,m=0;<binary>\x1b\\b";
    assert_eq!(sanitize_fresh(payload), b"ab");
}

#[test]
fn sos_payload_is_stripped() {
    let payload = b"a\x1bXattacker text\x1b\\b";
    assert_eq!(sanitize_fresh(payload), b"ab");
}

#[test]
fn pm_payload_is_stripped() {
    let payload = b"a\x1b^privacy message content\x1b\\b";
    assert_eq!(sanitize_fresh(payload), b"ab");
}

#[test]
fn consecutive_hostile_sequences_all_stripped() {
    let payload =
        b"\x1b]0;A\x07\x1b]52;c;XX\x07\x1bPq0\x1b\\\x1b_apc\x1b\\\x1bXsos\x1b\\\x1b^pm\x1b\\";
    let out = sanitize_fresh(payload);
    assert_eq!(out, b"");
    assert_no_control_escape_bytes(&out);
}

#[test]
fn interleaved_hostile_and_benign() {
    let payload = b"\x1b[31mHELLO\x1b[0m\x1b]0;T\x07 world\x1b]52;c;x\x07!";
    let out = sanitize_fresh(payload);
    assert_eq!(out, b"\x1b[31mHELLO\x1b[0m world!");
}

// ---- Benign streams (MUST pass through unchanged) ----

#[test]
fn plain_hello_world_survives() {
    let input = b"Hello, world!\n";
    assert_eq!(sanitize_fresh(input), input);
}

#[test]
fn colored_ls_output_survives() {
    // Typical `ls --color=always` output.
    let input = b"\x1b[0m\x1b[01;34mdir\x1b[0m\nfile.txt\n";
    assert_eq!(sanitize_fresh(input), input);
}

#[test]
fn cursor_movement_and_erase_survive() {
    let input = b"\x1b[2J\x1b[H\x1b[5;10Hx\x1b[K";
    assert_eq!(sanitize_fresh(input), input);
}

#[test]
fn utf8_multibyte_survives() {
    let input = "héllo 你好 🌍\n".as_bytes();
    assert_eq!(sanitize_fresh(input), input);
}

#[test]
fn emoji_adjacent_to_hostile_osc_is_preserved() {
    // Regression: early sanitizer drafts treated 0x9F (a byte that appears
    // inside 🌍 = F0 9F 8C 8D) as C1 APC and ate the emoji. Must NOT.
    let input = b"\xf0\x9f\x8c\x8d\x1b]0;HIJACK\x07\xf0\x9f\x8c\x8d";
    let out = sanitize_fresh(input);
    assert_eq!(out, b"\xf0\x9f\x8c\x8d\xf0\x9f\x8c\x8d");
}

#[test]
fn cr_lf_and_tab_survive() {
    let input = b"line1\r\n\tindented\r\nline3\n";
    assert_eq!(sanitize_fresh(input), input);
}

#[test]
fn decsc_decrc_keypad_survive() {
    // ESC 7 = save cursor; ESC 8 = restore; ESC = = keypad app mode.
    // None of these are string introducers and must not be stripped.
    let input = b"\x1b7x\x1b8\x1b=\x1b>";
    assert_eq!(sanitize_fresh(input), input);
}

// ---- Streaming across chunk boundaries ----

#[test]
fn osc_split_across_two_chunks() {
    let mut s = OutputSanitizer::new();
    let mut out = Vec::new();
    s.sanitize_into(b"keep\x1b]0;hal", &mut out);
    s.sanitize_into(b"f-title\x07tail", &mut out);
    assert_eq!(out, b"keeptail");
}

#[test]
fn osc_split_across_three_chunks_with_esc_terminator() {
    let mut s = OutputSanitizer::new();
    let mut out = Vec::new();
    s.sanitize_into(b"A\x1b]52", &mut out);
    s.sanitize_into(b";c;payload", &mut out);
    // ST delivered alone at chunk boundary.
    s.sanitize_into(b"\x1b", &mut out);
    s.sanitize_into(b"\\B", &mut out);
    assert_eq!(out, b"AB");
}

#[test]
fn dcs_tmux_split_across_chunks() {
    let mut s = OutputSanitizer::new();
    let mut out = Vec::new();
    s.sanitize_into(b"pre\x1bPtmu", &mut out);
    s.sanitize_into(b"x;\x1b]0;TITLE\x07", &mut out);
    s.sanitize_into(b"\x1b\\post", &mut out);
    assert_eq!(out, b"prepost");
}

#[test]
fn csi_split_across_chunks_preserves_both_halves() {
    let mut s = OutputSanitizer::new();
    let mut out = Vec::new();
    s.sanitize_into(b"a\x1b", &mut out);
    s.sanitize_into(b"[31mRED\x1b[0m", &mut out);
    assert_eq!(out, b"a\x1b[31mRED\x1b[0m");
}

#[test]
fn trailing_esc_at_end_of_stream_is_dropped_not_emitted() {
    // If the PTY closes with a half-pending ESC, the trailing byte is
    // held back until the next sanitize() call. This is the correct
    // behavior: emitting a lone ESC would be meaningless and risks
    // preceding garbage in the subsequent stream.
    let mut s = OutputSanitizer::new();
    let out = s.sanitize(b"done\x1b");
    assert_eq!(out, b"done");
    assert!(s.in_sequence());
}

// ---- Fail-closed properties ----

#[test]
fn stripped_byte_counter_monotonically_increases() {
    let mut s = OutputSanitizer::new();
    assert_eq!(s.stripped_bytes(), 0);
    let _ = s.sanitize(b"\x1b]0;X\x07");
    let first = s.stripped_bytes();
    assert!(first > 0);
    let _ = s.sanitize(b"safe");
    assert_eq!(s.stripped_bytes(), first);
    let _ = s.sanitize(b"\x1b]52;c;Y\x07");
    assert!(s.stripped_bytes() > first);
}

#[test]
fn no_escape_byte_ever_leaks_from_pure_hostile_input() {
    // Property: for any input consisting entirely of OSC/DCS/APC/SOS/PM
    // sequences, the output must be empty and contain no ESC/BEL/C1 bytes.
    let hostile = [
        &b"\x1b]0;x\x07"[..],
        &b"\x1b]52;c;AAAA\x07"[..],
        &b"\x1b]8;;url\x1b\\"[..],
        &b"\x1bPtmux;x\x1b\\"[..],
        &b"\x1b_apc\x1b\\"[..],
        &b"\x1bXsos\x1b\\"[..],
        &b"\x1b^pm\x1b\\"[..],
    ];
    for (idx, payload) in hostile.iter().enumerate() {
        let out = sanitize_fresh(payload);
        assert!(
            out.is_empty(),
            "payload #{idx} {payload:?} leaked bytes: {out:?}",
        );
        assert_no_control_escape_bytes(&out);
    }
}
