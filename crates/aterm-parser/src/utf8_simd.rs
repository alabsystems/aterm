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
#[path = "utf8_simd_tests.rs"]
mod tests;
