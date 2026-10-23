// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0
// Author: Andrew Yates

//! UTF-8 fast-path parity between bulk decoding (`decode_multibyte_run`), byte-by-byte
//! continuation decoding (`process_utf8_byte`), and `std`.

use super::super::*;
use super::RecordingSink;

fn print_whole(bytes: &[u8]) -> Vec<char> {
    let mut p = Parser::new();
    let mut s = RecordingSink::default();
    p.advance_fast(bytes, &mut s);
    s.prints
}

fn print_byte_by_byte(bytes: &[u8]) -> Vec<char> {
    let mut p = Parser::new();
    let mut s = RecordingSink::default();
    for &b in bytes {
        p.advance_fast(&[b], &mut s);
    }
    s.prints
}

#[test]
fn bulk_path_matches_continuation_path_for_sampled_codepoints() {
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
    assert_eq!(print_whole(s.as_bytes()), expected);
    assert_eq!(print_byte_by_byte(s.as_bytes()), expected);
}

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
        let n = (next(&mut state) % 40) as usize;
        let mut s = String::new();
        while s.chars().count() < n {
            let cp = next(&mut state) % 0x11_0000;
            if let Some(c) = char::from_u32(cp) {
                // Printable only: no C0 or C1 controls, no DEL.
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
fn incomplete_trailing_lead_after_complete_char_matches_bytewise() {
    let straddles: &[&[u8]] = &[
        &[0xE4, 0xB8, 0xAD, 0xE4],             // 中 + a lone trailing 3-byte lead
        &[0xE4, 0xB8, 0xAD, 0xF0, 0x9F],       // 中 + a truncated 4-byte lead
        &[0xC3, 0xA9, 0xC3],                   // é + a lone trailing 2-byte lead
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
    assert_eq!(print_whole(&[0xE4, 0xB8, 0xAD, 0xE4]), vec!['中']);
    assert_eq!(
        print_whole(&[0xE4, 0xB8, 0xAD, 0xE4, 0x41]),
        vec!['中', char::REPLACEMENT_CHARACTER, 'A']
    );
}

#[test]
fn ill_formed_utf8_is_replaced_per_maximal_subpart() {
    let cases: [&[u8]; 10] = [
        b"\xC0\x80Z",                         // overlong 2-byte lead: never valid
        b"\xC1\xBFZ",                         // overlong 2-byte lead: never valid
        b"\xE0\x80\x80Z",                     // overlong 3-byte: E0 needs A0..=BF
        b"\xED\xA0\x80Z",                     // surrogate U+D800: ED needs 80..=9F
        b"\xF0\x80\x80\x80Z",                 // overlong 4-byte: F0 needs 90..=BF
        b"\xF4\x90\x80\x80Z",                 // U+110000: F4 needs 80..=8F
        b"\xF5\x80\x80\x80Z",                 // F5 begins nothing
        b"\xE0\x80AZ",                        // a broken overlong prefix, then ASCII
        b"\xED\xA0\xBD\xED\xB8\x80Z",         // CESU-8 U+1F600 (two surrogates)
        b"\xE4\xB8\xAD\xE0\x9F\xE6\x96\x87Z", // 中, a broken prefix, 文
    ];
    for bytes in cases {
        let want: Vec<char> = String::from_utf8_lossy(bytes).chars().collect();
        assert_eq!(print_whole(bytes), want, "whole {bytes:02x?}");
        assert_eq!(print_byte_by_byte(bytes), want, "byte by byte {bytes:02x?}");
    }

    let mut state = 0x5DEE_CE66_D1CE_4E5Bu64;
    for _ in 0..20_000 {
        let mut bytes = Vec::new();
        for _ in 0..(state % 12) {
            state ^= state << 13;
            state ^= state >> 7;
            state ^= state << 17;
            let b = (state >> 24) as u8;
            bytes.push(if b < 0x20 || b == 0x7F { b | 0x80 } else { b });
        }
        bytes.push(b'Z');
        let want: Vec<char> = String::from_utf8_lossy(&bytes).chars().collect();
        assert_eq!(print_whole(&bytes), want, "whole {bytes:02x?}");
        assert_eq!(
            print_byte_by_byte(&bytes),
            want,
            "byte by byte {bytes:02x?}"
        );
    }
}
