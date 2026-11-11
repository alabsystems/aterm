// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

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
