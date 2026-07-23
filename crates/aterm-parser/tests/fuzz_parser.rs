// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0
//
// Robustness fuzz for the terminal escape-sequence parser. The parser processes
// untrusted bytes emitted by EVERY program a terminal runs, so it must NEVER
// panic on arbitrary input — a panic here is a denial-of-service (any program
// could crash the terminal by emitting a crafted byte sequence). This is the
// single highest-value daily-driver robustness surface.

use aterm_parser::{NullSink, Parser};

/// Deterministic LCG fuzz: 200k pseudo-random chunks driven through the full
/// state machine (CSI / OSC / DCS / ESC / UTF-8 / C0 / C1) via both `advance`
/// and `advance_fast`, with periodic resets. State carries across chunks, so
/// split/partial sequences are exercised too. Any panic aborts the test.
#[test]
fn fuzz_advance_never_panics() {
    let mut state: u64 = 0x243F_6A88_85A3_08D3;
    let mut next = move || {
        state = state
            .wrapping_mul(6_364_136_223_846_793_005)
            .wrapping_add(1_442_695_040_888_963_407);
        (state >> 33) as u32
    };
    let mut parser = Parser::new();
    let mut sink = NullSink;
    for i in 0..200_000u32 {
        let len = (next() % 64) as usize;
        let input: Vec<u8> = (0..len).map(|_| (next() & 0xFF) as u8).collect();
        if i & 1 == 0 {
            parser.advance(&input, &mut sink);
        } else {
            parser.advance_fast(&input, &mut sink);
        }
        // Occasionally reset to also cover fresh-state entry paths.
        if next() % 101 == 0 {
            parser.reset();
        }
    }
}

/// Biased fuzz toward real escape-sequence shapes — `ESC`, `CSI` (`ESC [`),
/// `OSC` (`ESC ]`), `DCS` (`ESC P`) introducers followed by random
/// parameter/payload bytes — to drive deeper into each sub-state-machine where
/// panics are most likely to hide.
#[test]
fn fuzz_escape_shapes_never_panic() {
    let mut state: u64 = 0xB7E1_5162_8AED_2A6B;
    let mut next = move || {
        state = state
            .wrapping_mul(6_364_136_223_846_793_005)
            .wrapping_add(1_442_695_040_888_963_407);
        (state >> 33) as u32
    };
    let intros: [&[u8]; 5] = [b"\x1b[", b"\x1b]", b"\x1bP", b"\x1b", b"\x1b_"];
    let mut parser = Parser::new();
    let mut sink = NullSink;
    for _ in 0..200_000u32 {
        let intro = intros[(next() % intros.len() as u32) as usize];
        let mut input: Vec<u8> = intro.to_vec();
        let body = (next() % 48) as usize;
        input.extend((0..body).map(|_| (next() & 0xFF) as u8));
        parser.advance(&input, &mut sink);
        if next() % 23 == 0 {
            parser.reset();
        }
    }
}
