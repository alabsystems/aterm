// Copyright 2026 Andrew Yates
// Author: Andrew Yates
// SPDX-License-Identifier: Apache-2.0

//! Runtime invariant tests across representative parser states and two-byte tails.

use super::super::*;

fn reachable_starts() -> Vec<(Vec<u8>, State)> {
    let full_params = |intro: &[u8]| [intro, &b"1;".repeat(MAX_PARAMS)].concat();
    let starts: &[(&[u8], State)] = &[
        (b"", State::Ground),
        (b"\x1b", State::Escape),
        (b"\x1b ", State::EscapeIntermediate),
        (b"\x1b    ", State::EscapeIntermediate),
        (b"\x1b[", State::CsiEntry),
        (b"\x1b[1", State::CsiParam),
        (b"\x1b[99999999999", State::CsiParam),
        (b"\x1b[1:2", State::CsiParam),
        (b"\x1b[?1", State::CsiParam),
        (b"\x1b[ ", State::CsiIntermediate),
        (b"\x1b[1    ", State::CsiIntermediate),
        (b"\x1b[1<", State::CsiIgnore),
        (b"\x1bP", State::DcsEntry),
        (b"\x1bP1", State::DcsParam),
        (b"\x1bP ", State::DcsIntermediate),
        (b"\x1bP    ", State::DcsIntermediate),
        (b"\x1bPq", State::DcsPassthrough),
        (b"\x1bPq\xe6\xbc", State::DcsPassthrough),
        (b"\x1bP:", State::DcsIgnore),
        (b"\x1b]", State::OscString),
        (b"\x1b]0;title", State::OscString),
        (b"\x1b_", State::SosPmApcString),
        (b"\x1b_\xe6", State::SosPmApcString),
        (b"\x1bX", State::SosPmApcString),
        (b"\x1b^", State::SosPmApcString),
        (b"\x1bPt", State::DcsIgnore),
        (b"\x1bPtmux", State::DcsIgnore),
        (b"\x1bPtmux;", State::Ground),
        (b"\x1bPtmux;\x1b", State::Ground),
        (b"\x1bPtmux;\x1b\x1bPq", State::DcsPassthrough),
        (b"\x1bPtmux;\x1b\x1bPq\x1b\x1b", State::Escape),
    ];
    let mut out: Vec<(Vec<u8>, State)> = starts
        .iter()
        .map(|&(bytes, state)| (bytes.to_vec(), state))
        .collect();
    out.push((full_params(b"\x1b["), State::CsiParam));
    out.push((full_params(b"\x1bP"), State::DcsParam));
    for lead in 0xC0..=0xF7u8 {
        out.push((vec![lead], State::Ground));
    }
    for (lead, low, high) in [
        (0xE0u8, 0xA0u8, 0xBFu8),
        (0xE1, 0x80, 0xBF),
        (0xED, 0x80, 0x9F),
        (0xEE, 0x80, 0xBF),
        (0xF0, 0x90, 0xBF),
        (0xF1, 0x80, 0xBF),
        (0xF4, 0x80, 0x8F),
    ] {
        for (second, third) in [(low, 0x80), (high, 0xBF)] {
            out.push((vec![lead, second], State::Ground));
            if lead >= 0xF0 {
                out.push((vec![lead, second, third], State::Ground));
            }
        }
    }
    out
}

fn assert_invariants_at(parser: &Parser, at: impl Fn() -> String) {
    if std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| parser.assert_invariants()))
        .is_err()
    {
        panic!("the invariant broke at {}", at());
    }
}

#[test]
fn every_state_every_byte_keeps_the_type_invariant() {
    let starts = reachable_starts();
    for c1 in [false, true] {
        let feed = |parser: &mut Parser, bytes: &[u8]| {
            parser.advance_fast(bytes, &mut NullSink);
        };
        for (prefix, state) in &starts {
            let mut start = if c1 {
                Parser::with_c1_controls()
            } else {
                Parser::new()
            };
            feed(&mut start, prefix);
            let at = || format!("c1={c1} {prefix:02x?}");
            assert_eq!(start.state(), *state, "{} reaches {state:?}", at());
            if *state == State::Ground && crate::utf8::Utf8Prefix::from_bytes(prefix).is_some() {
                assert_eq!(usize::from(start.utf8_len), prefix.len(), "{} pends", at());
            }
            assert_invariants_at(&start, at);
            for first in 0..=u8::MAX {
                let mut one = start.clone();
                feed(&mut one, &[first]);
                let at = || format!("c1={c1} {prefix:02x?} + {first:02x}");
                assert_invariants_at(&one, at);
                if first == 0x18 || first == 0x1A {
                    assert_eq!(one.state(), State::Ground, "{}", at());
                }
                if !c1
                    && *state == State::Ground
                    && start.tmux_passthrough() != TmuxPassthrough::Escape
                    && (0x80..=0x9F).contains(&first)
                {
                    assert_eq!(one.state(), State::Ground, "{}", at());
                }
                for second in 0..=u8::MAX {
                    let mut two = one.clone();
                    feed(&mut two, &[second]);
                    assert_invariants_at(&two, || {
                        format!("c1={c1} {prefix:02x?} + {first:02x} {second:02x}")
                    });
                }
            }
        }
    }
}
