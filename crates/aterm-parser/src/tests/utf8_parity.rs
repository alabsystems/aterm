// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0
// Author: Andrew Yates

//! UTF-8 fast-path PARITY (M2 SIMD-UTF8 verification).
//!
//! The ground-state hot loop decodes multi-byte UTF-8 in two ways: a BULK run
//! decoder (`decode_multibyte_run`, batching consecutive non-ASCII chars) and a
//! byte-by-byte CONTINUATION path (`process_utf8_byte`, used when a sequence
//! straddles an `advance()` boundary). These tests pin both against the `std`
//! reference: for every valid UTF-8 input the chars the parser prints must EXACTLY
//! equal `input.chars()`, whether fed whole or one byte at a time — and arbitrary
//! bytes must never panic.

use super::super::*;
use super::RecordingSink;

/// Feed `bytes` through a fresh parser in one `advance_fast` call; return printed
/// chars. `advance_fast` is the production UTF-8 path (with the bulk run decoder);
/// plain `advance` treats high bytes as Latin-1 and is NOT the path under test.
fn print_whole(bytes: &[u8]) -> Vec<char> {
    let mut p = Parser::new();
    let mut s = RecordingSink::default();
    p.advance_fast(bytes, &mut s);
    s.prints
}

/// Feed `bytes` one byte per `advance_fast` call through ONE parser (state
/// persists); return printed chars. Exercises the byte-by-byte continuation path
/// (a multi-byte sequence straddling `advance` boundaries).
fn print_byte_by_byte(bytes: &[u8]) -> Vec<char> {
    let mut p = Parser::new();
    let mut s = RecordingSink::default();
    for &b in bytes {
        p.advance_fast(&[b], &mut s);
    }
    s.prints
}

#[test]
fn valid_utf8_strings_roundtrip_exactly() {
    let samples = [
        "hello world",               // ASCII (SIMD bulk path)
        "café résumé naïve",         // 2-byte Latin
        "Ελληνικά Кириллица",        // 2-byte Greek/Cyrillic
        "日本語 中文 한국어",        // 3-byte CJK/Hangul
        "emoji: 😀🎉🚀 and 🇺🇸 flag", // 4-byte SMP + ZWJ flag
        "math ∑∫∞ ≠ ⊕ symbols",      // 3-byte symbols
        "mixed: aあb😀c中d",         // alternating 1/3/4-byte
    ];
    for s in samples {
        let expected: Vec<char> = s.chars().collect();
        assert_eq!(print_whole(s.as_bytes()), expected, "whole: {s:?}");
        assert_eq!(
            print_byte_by_byte(s.as_bytes()),
            expected,
            "byte-by-byte: {s:?}"
        );
    }
}

#[test]
fn bulk_path_matches_continuation_path_for_sampled_codepoints() {
    // One representative codepoint from every consequential UTF-8 boundary class.
    let cps: &[u32] = &[
        0x20, 0x7E, // printable ASCII edges (0x7F is DEL, a control — not printed)
        0x80, 0x7FF, // 2-byte edges
        0x800, 0xD7FF, // 3-byte below surrogates
        0xE000, 0xFFFF, // 3-byte above surrogates
        0x10000, 0x10FFFF, // 4-byte edges
        0x4E2D, 0x1F600, 0x00E9, 0x2211, // 中, 😀, é, ∑
    ];
    let s: String = cps.iter().filter_map(|&c| char::from_u32(c)).collect();
    let expected: Vec<char> = s.chars().collect();
    // Both paths must agree with each other AND with std's char iteration.
    assert_eq!(print_whole(s.as_bytes()), expected);
    assert_eq!(print_byte_by_byte(s.as_bytes()), expected);
}

/// Deterministic LCG (same constants as the engine fuzz) so failures reproduce.
fn next(state: &mut u64) -> u32 {
    *state = state
        .wrapping_mul(6_364_136_223_846_793_005)
        .wrapping_add(1_442_695_040_888_963_407);
    (*state >> 33) as u32
}

#[test]
fn fuzz_random_valid_utf8_has_exact_parity() {
    let mut state = 0xC0FFEE_1234_5678u64;
    for _ in 0..3_000 {
        // Build a random string of valid scalar values (char::from_u32 excludes
        // surrogates + out-of-range), avoiding ESC/CSI so we stay on the print path.
        let n = (next(&mut state) % 40) as usize;
        let mut s = String::new();
        while s.chars().count() < n {
            let cp = next(&mut state) % 0x11_0000;
            if let Some(c) = char::from_u32(cp) {
                // Skip C0/C1 controls + ESC so the bytes stay in the printable path.
                if c >= ' ' && c != '\x7f' && !('\u{80}'..='\u{9f}').contains(&c) {
                    s.push(c);
                }
            }
        }
        let expected: Vec<char> = s.chars().collect();
        assert_eq!(print_whole(s.as_bytes()), expected, "whole parity: {s:?}");
        assert_eq!(
            print_byte_by_byte(s.as_bytes()),
            expected,
            "split parity: {s:?}"
        );
    }
}

#[test]
fn fuzz_arbitrary_bytes_never_panic() {
    // The fast path parses ATTACKER bytes; arbitrary input must never panic, and
    // whole vs byte-by-byte feeding must never disagree on length-0 vs crash.
    let mut state = 0xDEAD_BEEF_F00Du64;
    for _ in 0..5_000 {
        let len = (next(&mut state) % 64) as usize;
        let buf: Vec<u8> = (0..len).map(|_| next(&mut state) as u8).collect();
        // Both must complete without panicking.
        let _ = print_whole(&buf);
        let _ = print_byte_by_byte(&buf);
    }
}

#[test]
fn invalid_sequences_emit_replacement_not_garbage() {
    // A lone lead byte followed by ASCII: replacement, then the ASCII prints.
    let out = print_whole(b"\xE4Z"); // truncated 3-byte lead + 'Z'
    assert!(
        out.contains(&char::REPLACEMENT_CHARACTER),
        "truncated lead must yield U+FFFD, got {out:?}"
    );
    assert!(out.contains(&'Z'), "trailing ASCII must still print");
    // An overlong 2-byte encoding of '/' (0xC0 0xAF) must NOT decode to '/'.
    let overlong = print_whole(b"\xC0\xAF");
    assert!(
        !overlong.contains(&'/'),
        "overlong encoding must not produce '/', got {overlong:?}"
    );
}

/// THRU-4a regression (whole-vs-bytewise parity for the read-boundary straddle):
/// a chunk that ends mid-sequence AFTER at least one complete multibyte char must
/// decode identically whether fed whole or one byte at a time. The bulk lane's
/// scalar boundary step must NOT double-consume the trailing incomplete lead —
/// it is buffered for the next chunk's continuation, not replaced with U+FFFD.
/// (Before the fix, `print_whole(&[E4 B8 AD E4])` emitted `['中','\u{FFFD}']`
/// while the correct byte-by-byte path emitted `['中']`.)
#[test]
fn incomplete_trailing_lead_after_complete_char_matches_bytewise() {
    let straddles: &[&[u8]] = &[
        &[0xE4, 0xB8, 0xAD, 0xE4], // 中 + a lone trailing 3-byte lead
        &[0xE4, 0xB8, 0xAD, 0xF0, 0x9F], // 中 + a truncated 4-byte lead
        &[0xC3, 0xA9, 0xC3],       // é + a lone trailing 2-byte lead
        &[0xF0, 0x9F, 0x98, 0x80, 0xE4, 0xB8], // 😀 + a truncated 3-byte lead
        &[0xE4, 0xB8, 0xAD, 0xE4, 0xB8, 0xAD, 0xE4], // 中中 + a lone 3-byte lead
    ];
    for &bytes in straddles {
        assert_eq!(
            print_whole(bytes),
            print_byte_by_byte(bytes),
            "whole vs byte-by-byte diverged on {bytes:02x?}"
        );
    }
    // Pin the canonical case exactly: the complete prefix prints, the incomplete
    // trailing lead prints NOTHING (it is buffered), so only 中 appears.
    assert_eq!(print_whole(&[0xE4, 0xB8, 0xAD, 0xE4]), vec!['中']);
    // A NON-continuation byte after a mid-run lead: exactly ONE U+FFFD (for the
    // invalid lead) then the ASCII — never two replacements.
    assert_eq!(
        print_whole(&[0xE4, 0xB8, 0xAD, 0xE4, 0x41]),
        vec!['中', char::REPLACEMENT_CHARACTER, 'A']
    );
}
