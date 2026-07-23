// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! THRU-4a — the never-done half of SIMD-UTF8: a bulk validate+decode fast lane
//! for HOMOGENEOUS multibyte runs (long stretches of same-length sequences —
//! CJK/Hangul/Greek/Cyrillic 3-byte, Latin-extended 2-byte, SMP-emoji 4-byte),
//! the shape that dominates the `cjk`/`mixed` and `wide_unicode` corpora.
//!
//! It is an ADDITIVE fast lane, not a replacement: [`bulk_decode_run`] consumes
//! only the maximal prefix of the input that is one uniformly-valid sequence
//! class, and STOPS — returning the bytes consumed so far — at the first byte it
//! cannot take (a class change, an invalid/overlong/surrogate sequence, an
//! incomplete tail, or a full output buffer). The caller (`decode_multibyte_run`
//! in `dispatch.rs`) handles that one boundary byte with the scalar
//! `decode_multibyte_char` oracle and then re-enters this lane, so the byte-exact
//! decoding semantics — including the U+FFFD replacement policy for malformed
//! input — remain entirely the scalar path's, proven by the `utf8_parity` /
//! `utf8_errors` suites and the `bulk_matches_scalar_*` differential tests here.
//!
//! WHY THIS IS FASTER: each per-class loop is monomorphic — it drops the
//! three-way lead-class dispatch and the per-character "is the next byte still a
//! multibyte lead?" re-check that the general scalar loop pays every iteration,
//! leaving a tight fixed-stride body (continuation-byte mask + shift/or codepoint
//! assembly) the branch predictor and LLVM's autovectorizer both handle far
//! better. It is SAFE code with no `std::arch` intrinsics — the sanctioned
//! aarch64 idiom (the Trust verifier fails closed on unmodeled intrinsics), so
//! it stays inside the always-on verification gate; the hand-NEON variant is the
//! separate, proof-carried THRU-4(b).

use aterm_alloc::ArrayVec;

/// Multibyte output batch cap — must match `dispatch.rs`'s `ArrayVec<char, 256>`.
pub(crate) const RUN_CAP: usize = 256;

/// Decode a maximal run of consecutive, fully-valid, SAME-LENGTH UTF-8 multibyte
/// sequences from the front of `bytes`, appending decoded `char`s to `out` (never
/// past its [`RUN_CAP`] capacity). Returns the number of bytes consumed.
///
/// Dispatches on the class of `bytes[0]` and defers to the matching fixed-stride
/// loop. Returns `0` (consumes nothing) when `bytes` is empty, does not start on
/// a multibyte lead, or its first sequence is not fully valid — in every such
/// case the caller's scalar boundary step makes progress, so no caller loop can
/// spin. Because each loop's acceptance test is byte-for-byte the scalar
/// `decode_multibyte_char`'s, the chars produced here are exactly the chars the
/// scalar path would produce for the bytes consumed.
pub(crate) fn bulk_decode_run(bytes: &[u8], out: &mut ArrayVec<char, RUN_CAP>) -> usize {
    match bytes.first() {
        Some(&b0) if (0xE0..=0xEF).contains(&b0) => bulk_3byte(bytes, out),
        Some(&b0) if (0xC0..=0xDF).contains(&b0) => bulk_2byte(bytes, out),
        Some(&b0) if (0xF0..=0xF7).contains(&b0) => bulk_4byte(bytes, out),
        _ => 0,
    }
}

/// 3-byte run (BMP non-ASCII: CJK, Hangul, Greek, Cyrillic, …) — the dominant
/// class in the target corpora.
fn bulk_3byte(bytes: &[u8], out: &mut ArrayVec<char, RUN_CAP>) -> usize {
    let mut i = 0usize;
    while !out.is_full() {
        // Total, panic-free: a slice of exactly 3 or nothing (incomplete tail).
        let Some(&[b0, b1, b2]) = bytes.get(i..i + 3) else {
            break;
        };
        // Class boundary: only 0xE0..=0xEF continue this run.
        if !(0xE0..=0xEF).contains(&b0) {
            break;
        }
        // Continuation bytes.
        if (b1 & 0xC0) != 0x80 || (b2 & 0xC0) != 0x80 {
            break;
        }
        let cp =
            (u32::from(b0 & 0x0F) << 12) | (u32::from(b1 & 0x3F) << 6) | u32::from(b2 & 0x3F);
        // Overlong (< 0x800) and surrogates (0xD800..=0xDFFF) are handed back to
        // the scalar path, which emits the U+FFFD replacement exactly as before.
        if cp < 0x800 || (0xD800..=0xDFFF).contains(&cp) {
            break;
        }
        // cp is in 0x800..=0xFFFF and non-surrogate ⇒ a valid scalar value, so
        // `from_u32` is always `Some`; `unwrap_or` never yields the fallback (it
        // exists only to keep the loop free of `unwrap`/panic obligations).
        out.push(char::from_u32(cp).unwrap_or('\u{FFFD}'));
        i += 3;
    }
    i
}

/// 2-byte run (Latin-1 supplement, Latin Extended, IPA, Greek, Cyrillic in the
/// 0x80..=0x7FF range).
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
        // Overlong 2-byte forms (leads 0xC0/0xC1 ⇒ cp < 0x80) go to the scalar path.
        if cp < 0x80 {
            break;
        }
        // 0x80..=0x7FF are all valid scalar values.
        out.push(char::from_u32(cp).unwrap_or('\u{FFFD}'));
        i += 2;
    }
    i
}

/// 4-byte run (SMP: emoji, mathematical alphanumerics, historic scripts).
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
        // Overlong (< 0x10000) and above-Unicode (> 0x10FFFF) go to the scalar path.
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

    /// The scalar oracle: decode ONE multibyte char exactly as `dispatch.rs`'s
    /// `decode_multibyte_char` does, so the differential tests compare against
    /// the shipping semantics (kept a byte-for-byte copy on purpose).
    fn scalar_one(lead: u8, rem: &[u8]) -> Option<(char, usize)> {
        if lead >= 0xF0 {
            if let &[c0, c1, c2, ..] = rem
                && (c0 & 0xC0) == 0x80
                && (c1 & 0xC0) == 0x80
                && (c2 & 0xC0) == 0x80
            {
                let cp = (u32::from(lead & 0x07) << 18)
                    | (u32::from(c0 & 0x3F) << 12)
                    | (u32::from(c1 & 0x3F) << 6)
                    | u32::from(c2 & 0x3F);
                if (0x10000..=0x0010_FFFF).contains(&cp) {
                    return char::from_u32(cp).map(|c| (c, 3));
                }
            }
            None
        } else if lead >= 0xE0 {
            if let &[c0, c1, ..] = rem
                && (c0 & 0xC0) == 0x80
                && (c1 & 0xC0) == 0x80
            {
                let cp = (u32::from(lead & 0x0F) << 12)
                    | (u32::from(c0 & 0x3F) << 6)
                    | u32::from(c1 & 0x3F);
                if cp >= 0x800 && !(0xD800..=0xDFFF).contains(&cp) {
                    return char::from_u32(cp).map(|c| (c, 2));
                }
            }
            None
        } else {
            if let &[c0, ..] = rem
                && (c0 & 0xC0) == 0x80
            {
                let cp = (u32::from(lead & 0x1F) << 6) | u32::from(c0 & 0x3F);
                if cp >= 0x80 {
                    return char::from_u32(cp).map(|c| (c, 1));
                }
            }
            None
        }
    }

    /// Reference: decode a homogeneous run one scalar char at a time, exactly the
    /// way the caller's boundary step would, and stop where `bulk_decode_run`
    /// must stop (class change / invalid / incomplete / full).
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
            match scalar_one(lead, bytes.get(i + 1..).unwrap_or(&[])) {
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
            "𝕏𝕐𝕑𝔸𝔹ℂ𝕆".repeat(3),              // 4-byte SMP math
            "🚀🎨🔥💧🌈🎉".repeat(3),           // 4-byte emoji
        ] {
            let (rc, ri) = ref_run(s.as_bytes());
            let (bc, bi) = bulk_run(s.as_bytes());
            assert_eq!((rc, ri), (bc, bi), "input {s:?}");
        }
    }

    #[test]
    fn bulk_stops_at_class_change() {
        // 3-byte CJK then a 2-byte lead: the run must stop at the class boundary.
        let s = "漢字".to_string() + "β";
        let (rc, ri) = ref_run(s.as_bytes());
        let (bc, bi) = bulk_run(s.as_bytes());
        assert_eq!((rc, ri), (bc, bi));
        // It consumed only the two 3-byte chars (6 bytes), leaving the 2-byte lead.
        assert_eq!(bi, 6);
    }

    #[test]
    fn bulk_stops_before_ascii_and_control() {
        for tail in ["A", "\n", " ", "\x1b"] {
            let s = format!("中文{tail}");
            let (rc, ri) = ref_run(s.as_bytes());
            let (bc, bi) = bulk_run(s.as_bytes());
            assert_eq!((rc, ri), (bc, bi), "tail {tail:?}");
            assert_eq!(bi, 6, "must stop before the ASCII/control byte");
        }
    }

    #[test]
    fn bulk_stops_on_incomplete_tail() {
        // A 3-byte lead with only one continuation byte present: consume nothing
        // of it (leave it for the scalar straddle path).
        let mut bytes = "中文".as_bytes().to_vec(); // 6 bytes, 2 chars
        bytes.extend_from_slice(&[0xE4, 0xB8]); // truncated 3-byte lead
        let (rc, ri) = ref_run(&bytes);
        let (bc, bi) = bulk_run(&bytes);
        assert_eq!((rc, ri), (bc, bi));
        assert_eq!(bi, 6);
    }

    #[test]
    fn bulk_rejects_overlong_and_surrogate_like_scalar() {
        // Overlong 3-byte 0xE0 0x80 0x80 (encodes < 0x800) and a surrogate
        // 0xED 0xA0 0x80 (U+D800) must both be left to the scalar path (0 taken).
        for bad in [[0xE0u8, 0x80, 0x80], [0xED, 0xA0, 0x80]] {
            let (rc, ri) = ref_run(&bad);
            let (bc, bi) = bulk_run(&bad);
            assert_eq!((rc.clone(), ri), (bc, bi), "bad {bad:?}");
            assert_eq!(bi, 0, "overlong/surrogate must not be consumed");
        }
    }

    #[test]
    fn bulk_rejects_overlong_two_byte() {
        // 0xC0 0x80 and 0xC1 0xBF are overlong ⇒ scalar-only.
        for bad in [[0xC0u8, 0x80], [0xC1, 0xBF]] {
            let (_, bi) = bulk_run(&bad);
            assert_eq!(bi, 0);
        }
    }

    #[test]
    fn bulk_respects_the_run_cap() {
        // 300 CJK chars but the batch caps at RUN_CAP: consume exactly RUN_CAP
        // sequences (RUN_CAP*3 bytes), leaving the rest for the next call.
        let s = "中".repeat(300);
        let (bc, bi) = bulk_run(s.as_bytes());
        assert_eq!(bc.len(), RUN_CAP);
        assert_eq!(bi, RUN_CAP * 3);
    }

    #[test]
    fn bulk_empty_and_non_lead_consume_nothing() {
        assert_eq!(bulk_run(&[]).1, 0);
        assert_eq!(bulk_run(b"hello").1, 0);
        assert_eq!(bulk_run(&[0x80, 0x80]).1, 0); // orphan continuations
        assert_eq!(bulk_run(&[0xF8, 0x80]).1, 0); // 0xF8 is not a valid lead
    }

    /// Exhaustive-ish differential fuzz: random byte buffers, bulk vs the scalar
    /// reference must agree on (chars, consumed) for every one.
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
                // Bias toward multibyte leads + continuations so runs actually form.
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
            assert_eq!(ref_run(&bytes), bulk_run(&bytes), "diverged on {bytes:02x?}");
        }
    }
}
