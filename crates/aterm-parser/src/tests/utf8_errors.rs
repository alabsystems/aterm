// Copyright 2026 Andrew Yates
// Author: Andrew Yates
// SPDX-License-Identifier: Apache-2.0

//! UTF-8 error and prefix paths: how `advance_fast` resynchronizes when a
//! control interrupts a pending character, and how the string bulk paths and a
//! carry track a string's pending UTF-8 character (`Utf8Prefix`).

use super::super::*;
use super::RecordingSink;

/// ESC interrupts an incomplete UTF-8 prefix: emit one replacement, then
/// interpret the escape sequence normally, including across input chunks.
#[test]
fn fast_invalid_utf8_prefix_replays_escape_across_chunks() {
    let input = [0xE2, 0x1B, b'7'];
    for split in 0..=input.len() {
        let mut parser = Parser::new();
        let mut sink = RecordingSink::default();
        parser.advance_fast(&input[..split], &mut sink);
        parser.advance_fast(&input[split..], &mut sink);

        assert_eq!(
            sink.prints,
            vec![char::REPLACEMENT_CHARACTER],
            "split {split}: one replacement for the incomplete prefix"
        );
        assert_eq!(
            sink.esc_dispatches,
            vec![(Vec::new(), b'7')],
            "split {split}: ESC is replayed as a control"
        );
        assert_eq!(parser.state(), State::Ground, "split {split}");
    }
}

/// `Utf8Prefix::after` (how the DCS and APC bulk paths bring a string's UTF-8
/// head up to date over a run, in O(1)) is `Utf8Prefix::step` over each byte
/// of the run, from every prefix a stream can end in, over every run of up to
/// four bytes drawn from one byte of each class Table 3-7 tells apart (ASCII,
/// the continuation sub-ranges the second-byte rules split, every kind of
/// lead, and the bytes that begin nothing). And every such prefix is the one
/// `Utf8Prefix::from_bytes` reads back from its bytes (how a carry restores
/// it), while a whole character, a broken prefix and a stray byte are not.
#[test]
fn a_prefix_after_a_run_is_the_prefix_after_each_byte() {
    use crate::utf8::Utf8Prefix;
    const CLASSES: [u8; 16] = [
        0x41, 0x80, 0x8F, 0x90, 0x9C, 0x9F, 0xA0, 0xBF, 0xC0, 0xDC, 0xE0, 0xE6, 0xED, 0xF0, 0xF4,
        0xF5,
    ];
    let mut runs: Vec<Vec<u8>> = vec![Vec::new()];
    for len in 1..=4 {
        let mut next = Vec::new();
        for run in runs.iter().filter(|run| run.len() == len - 1) {
            for &byte in &CLASSES {
                let mut longer = run.clone();
                longer.push(byte);
                next.push(longer);
            }
        }
        runs.extend(next);
    }
    let each = |start: Utf8Prefix, run: &[u8]| run.iter().fold(start, |p, &b| p.step(b));
    let mut starts: Vec<Utf8Prefix> = runs
        .iter()
        .filter(|run| run.len() <= 3)
        .map(|run| each(Utf8Prefix::NONE, run))
        .collect();
    starts.sort_by_key(|p| p.as_slice().to_vec());
    starts.dedup();
    assert!(starts.len() > 40, "{} prefixes", starts.len());
    for &start in &starts {
        assert_eq!(Utf8Prefix::from_bytes(start.as_slice()), Some(start));
        assert!(start.as_slice().len() < usize::from(start.expected().max(1)));
        for run in &runs {
            assert_eq!(
                start.after(run),
                each(start, run),
                "{start:02x?} then {run:02x?}"
            );
        }
    }
    for not_a_prefix in [
        &b"\xe6\x9c\xac"[..],
        b"\xdc\x9c",
        b"\xe0\x9c",
        b"\xf4\x90",
        b"\xe6x",
        b"\x9c",
        b"\xc0",
        b"\xf5",
        b"x",
    ] {
        assert_eq!(
            Utf8Prefix::from_bytes(not_a_prefix),
            None,
            "{not_a_prefix:02x?}"
        );
    }
}
