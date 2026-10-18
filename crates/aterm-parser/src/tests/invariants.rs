// Copyright 2026 Andrew Yates
// Author: Andrew Yates
// SPDX-License-Identifier: Apache-2.0

//! Runtime invariant tests: `assert_invariants` holds from every
//! representative state through every two-byte tail.

use super::super::*;

/// Representative states at every bound the invariant names: each of the 14
/// states, a full parameter list, a saturated parameter, a subparameter, a
/// full intermediate list, a string's split UTF-8 character, an OSC with
/// data and all three `SosPmApcString` introducers; and at `Ground`, every
/// lead byte `0xC0..=0xF7` (the ones that begin no sequence included), and
/// for each three- and four-byte row of Unicode Table 3-7 the character
/// pending with the lowest and with the highest second byte the row allows,
/// and for a four-byte row a third byte after either (`E0` overlong, `E1`
/// for `E1..=EC`, `ED` surrogate, `EE` for `EE..=EF`, `F0` and `F4` plane
/// bounds, `F1` for `F1..=F3`). No prefix holds a byte in 0x80..=0x9F
/// outside a character, so each reaches its state in both C1 modes.
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

/// Assert the invariant, naming the input that broke it.
fn assert_invariants_at(parser: &Parser, at: impl Fn() -> String) {
    if std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| parser.assert_invariants()))
        .is_err()
    {
        panic!("the invariant broke at {}", at());
    }
}

/// THE TYPE INVARIANT HOLDS FROM REPRESENTATIVE STATES AT EVERY BOUND THROUGH
/// EVERY TWO-BYTE TAIL. From each of [`reachable_starts`], with C1 controls
/// off and on, every byte and then every byte after it (65,792 tails), fed one
/// byte at a time, leaves the parser inside `assert_invariants` — the TLA+
/// `TypeInvariant`, the flag-state couplings, the UTF-8 decoder's consistency
/// and the string-head placement — after every byte: so every lead
/// `0xC0..=0xF7` meets every two following bytes, and each row's pending
/// prefixes every two bytes more, driven through `advance_fast` from the
/// states it reaches itself. Two transitions are pinned on the
/// way: CAN (0x18) and SUB (0x1A) end every sequence at `Ground`, and with C1
/// controls off no byte in 0x80..=0x9F moves a `Ground` parser.
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
                if !c1 && *state == State::Ground && (0x80..=0x9F).contains(&first) {
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
