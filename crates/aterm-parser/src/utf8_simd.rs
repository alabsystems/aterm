// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Bulk validate-and-decode for runs of same-length UTF-8 sequences.

use aterm_alloc::ArrayVec;

/// The run buffer's capacity (`decode_multibyte_run`'s `ArrayVec<char, 256>`).
pub(crate) const RUN_CAP: usize = 256;

/// Decode a leading run of valid same-length multibyte sequences into `out` and return the bytes consumed.
#[inline]
pub(crate) fn bulk_decode_run(bytes: &[u8], out: &mut ArrayVec<char, RUN_CAP>) -> usize {
    match bytes.first() {
        Some(&b0) if (0xE0..=0xEF).contains(&b0) => bulk_3byte(bytes, out),
        Some(&b0) if (0xC0..=0xDF).contains(&b0) => bulk_2byte(bytes, out),
        Some(&b0) if (0xF0..=0xF7).contains(&b0) => bulk_4byte(bytes, out),
        _ => 0,
    }
}

/// The longest printable-ASCII gap a multi-byte run joins: the spaces and punctuation between
/// words. A longer stretch ends the run and goes to `print_ascii_bulk`, whose writer takes ASCII
/// for less per byte than the run's per-character loop does; past about this length that costs
/// more than the run break it saves.
pub(crate) const JOIN_GAP_MAX: usize = 32;

/// The length of the printable ASCII (`0x20..=0x7E`) `bytes` begins with, when a multi-byte lead
/// ([`crate::utf8::is_utf8_lead`]) follows it within its first `room` bytes and its first
/// [`JOIN_GAP_MAX`] + 1: the space between two words a multi-byte run joins across. `0` when
/// another byte ends it, when it reaches either bound or the end of `bytes` (that ASCII stays
/// for the printable scan), and when `bytes` does not begin with printable ASCII. A nonzero gap
/// is shorter than `room`, so the lead's character fits after it. The scan is the printable
/// scanner's ([`crate::simd::count_printable`]) over at most that window, so a gap that does
/// not join costs little before the printable scan reads it again.
#[inline]
pub(crate) fn joinable_ascii_gap(bytes: &[u8], room: usize) -> usize {
    let window = bytes.get(..room.min(JOIN_GAP_MAX + 1)).unwrap_or(bytes);
    let gap = crate::simd::count_printable(window);
    match window.get(gap) {
        Some(&lead) if crate::utf8::is_utf8_lead(lead) => gap,
        _ => 0,
    }
}

/// 3-byte run (BMP: CJK, Hangul, Greek, Cyrillic, ...).
#[inline]
fn bulk_3byte(bytes: &[u8], out: &mut ArrayVec<char, RUN_CAP>) -> usize {
    let mut i = 0usize;
    while !out.is_full() {
        let Some(&[b0, b1, b2]) = bytes.get(i..i + 3) else {
            break;
        };
        if !(0xE0..=0xEF).contains(&b0) {
            break;
        }
        if (b1 & 0xC0) != 0x80 || (b2 & 0xC0) != 0x80 {
            break;
        }
        let cp = (u32::from(b0 & 0x0F) << 12) | (u32::from(b1 & 0x3F) << 6) | u32::from(b2 & 0x3F);
        if cp < 0x800 || (0xD800..=0xDFFF).contains(&cp) {
            break;
        }
        out.push(char::from_u32(cp).unwrap_or('\u{FFFD}'));
        i += 3;
    }
    i
}

/// 2-byte run (U+0080..=U+07FF: Latin extensions, IPA, Greek, Cyrillic).
#[inline]
fn bulk_2byte(bytes: &[u8], out: &mut ArrayVec<char, RUN_CAP>) -> usize {
    let mut i = 0usize;
    while !out.is_full() {
        let Some(&[b0, b1]) = bytes.get(i..i + 2) else {
            break;
        };
        if !(0xC0..=0xDF).contains(&b0) {
            break;
        }
        if (b1 & 0xC0) != 0x80 {
            break;
        }
        let cp = (u32::from(b0 & 0x1F) << 6) | u32::from(b1 & 0x3F);
        if cp < 0x80 {
            break;
        }
        out.push(char::from_u32(cp).unwrap_or('\u{FFFD}'));
        i += 2;
    }
    i
}

/// 4-byte run (SMP: emoji, mathematical alphanumerics, historic scripts).
#[inline]
fn bulk_4byte(bytes: &[u8], out: &mut ArrayVec<char, RUN_CAP>) -> usize {
    let mut i = 0usize;
    while !out.is_full() {
        let Some(&[b0, b1, b2, b3]) = bytes.get(i..i + 4) else {
            break;
        };
        if !(0xF0..=0xF7).contains(&b0) {
            break;
        }
        if (b1 & 0xC0) != 0x80 || (b2 & 0xC0) != 0x80 || (b3 & 0xC0) != 0x80 {
            break;
        }
        let cp = (u32::from(b0 & 0x07) << 18)
            | (u32::from(b1 & 0x3F) << 12)
            | (u32::from(b2 & 0x3F) << 6)
            | u32::from(b3 & 0x3F);
        if !(0x10000..=0x0010_FFFF).contains(&cp) {
            break;
        }
        out.push(char::from_u32(cp).unwrap_or('\u{FFFD}'));
        i += 4;
    }
    i
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used)]
    use super::*;

    fn ref_run(bytes: &[u8]) -> (Vec<char>, usize) {
        let mut out = Vec::new();
        let mut i = 0usize;
        let class = |b: u8| -> u8 {
            if (0xF0..=0xF7).contains(&b) {
                4
            } else if (0xE0..=0xEF).contains(&b) {
                3
            } else if (0xC0..=0xDF).contains(&b) {
                2
            } else {
                0
            }
        };
        let Some(&first) = bytes.first() else {
            return (out, 0);
        };
        let run_class = class(first);
        if run_class == 0 {
            return (out, 0);
        }
        while out.len() < RUN_CAP {
            let Some(&lead) = bytes.get(i) else { break };
            if class(lead) != run_class {
                break;
            }
            match crate::Parser::decode_multibyte_char(lead, bytes.get(i + 1..).unwrap_or(&[])) {
                Some((c, consumed)) => {
                    out.push(c);
                    i += 1 + consumed;
                }
                None => break,
            }
        }
        (out, i)
    }

    fn bulk_run(bytes: &[u8]) -> (Vec<char>, usize) {
        let mut out: ArrayVec<char, RUN_CAP> = ArrayVec::new();
        let consumed = bulk_decode_run(bytes, &mut out);
        (out.as_slice().to_vec(), consumed)
    }

    #[test]
    fn bulk_matches_scalar_homogeneous_cjk() {
        let s = "漢字測試中文寬字符壓力純解碼路徑".repeat(4);
        let (rc, ri) = ref_run(s.as_bytes());
        let (bc, bi) = bulk_run(s.as_bytes());
        assert_eq!((rc, ri), (bc, bi));
    }

    #[test]
    fn bulk_matches_scalar_two_and_four_byte() {
        for s in [
            "αβγδεζηθικλμνξοπρστυφχψω".repeat(3), // 2-byte Greek
            "𝕏𝕐𝕑𝔸𝔹ℂ𝕆".repeat(3),                  // 4-byte SMP math
            "🚀🎨🔥💧🌈🎉".repeat(3),             // 4-byte emoji
        ] {
            let (rc, ri) = ref_run(s.as_bytes());
            let (bc, bi) = bulk_run(s.as_bytes());
            assert_eq!((rc, ri), (bc, bi), "input {s:?}");
        }
    }

    #[test]
    fn a_gap_joins_up_to_the_longest_join_and_short_of_the_room() {
        let gap = |n: usize, room: usize| {
            let mut bytes = vec![b'x'; n];
            bytes.extend_from_slice("\u{e9}".as_bytes());
            joinable_ascii_gap(&bytes, room)
        };
        assert_eq!(gap(1, RUN_CAP), 1);
        assert_eq!(gap(JOIN_GAP_MAX, RUN_CAP), JOIN_GAP_MAX);
        assert_eq!(gap(JOIN_GAP_MAX + 1, RUN_CAP), 0);
        assert_eq!(gap(5, 6), 5, "the lead's character fits after the gap");
        assert_eq!(gap(5, 5), 0, "no room for the lead's character");
        assert_eq!(
            joinable_ascii_gap(b"ab\r\xc3\xa9", RUN_CAP),
            0,
            "a control ends it"
        );
        assert_eq!(joinable_ascii_gap(b"ab", RUN_CAP), 0, "no lead follows");
        assert_eq!(joinable_ascii_gap(b"\x80ab\xc3\xa9", RUN_CAP), 0);
    }

    #[test]
    fn bulk_respects_the_run_cap() {
        let s = "中".repeat(300);
        let (bc, bi) = bulk_run(s.as_bytes());
        assert_eq!(bc.len(), RUN_CAP);
        assert_eq!(bi, RUN_CAP * 3);
    }

    #[test]
    fn bulk_first_sequence_matches_scalar_for_every_lead_and_continuation_class() {
        const CLASSES: [u8; 10] = [0x00, 0x7F, 0x80, 0x8F, 0x90, 0x9F, 0xA0, 0xBF, 0xC0, 0xFF];
        for lead in 0..=u8::MAX {
            for second in 0..=u8::MAX {
                for third in CLASSES {
                    for fourth in CLASSES {
                        let input = [lead, second, third, fourth];
                        let mut out: ArrayVec<char, RUN_CAP> = ArrayVec::new();
                        let consumed = bulk_decode_run(&input, &mut out);
                        let oracle = crate::Parser::decode_multibyte_char(lead, &input[1..]);
                        match oracle {
                            Some((c, continuations)) => {
                                assert!(consumed > continuations, "{input:02x?}");
                                assert_eq!(out.as_slice().first(), Some(&c), "{input:02x?}");
                            }
                            None => {
                                assert_eq!(consumed, 0, "{input:02x?}");
                                assert!(out.is_empty(), "{input:02x?}");
                            }
                        }
                    }
                }
            }
        }
    }

    #[test]
    fn bulk_matches_scalar_on_random_buffers() {
        let mut state = 0x1234_5678_9abc_def0u64;
        let mut rand = || {
            state ^= state << 13;
            state ^= state >> 7;
            state ^= state << 17;
            state
        };
        for _ in 0..20_000 {
            let len = (rand() % 24) as usize;
            let mut bytes = Vec::with_capacity(len);
            for _ in 0..len {
                let r = rand();
                let b = match r % 5 {
                    0 => 0xE0 + (r >> 8) as u8 % 0x10, // 3-byte lead
                    1 => 0x80 + (r >> 8) as u8 % 0x40, // continuation
                    2 => 0xC2 + (r >> 8) as u8 % 0x1E, // 2-byte lead
                    3 => 0xF0 + (r >> 8) as u8 % 0x05, // 4-byte lead
                    _ => (r >> 8) as u8,               // anything
                };
                bytes.push(b);
            }
            assert_eq!(
                ref_run(&bytes),
                bulk_run(&bytes),
                "diverged on {bytes:02x?}"
            );
        }
    }
}
