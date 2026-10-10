// Copyright 2026 Andrew Yates
// Author: Andrew Yates
// SPDX-License-Identifier: Apache-2.0

//! Runtime invariant tests: verify assert_invariants holds after
//! every sequence type, partial sequences, resets, and edge cases.

use super::super::*;
use super::RecordingSink;

#[test]
fn assert_invariants_new_parser() {
    let parser = Parser::new();
    // Should not panic - fresh parser is in valid state
    parser.assert_invariants();
}

#[test]
fn assert_invariants_after_plain_text() {
    let mut parser = Parser::new();
    let mut sink = RecordingSink::default();

    parser.advance(b"Hello, World!", &mut sink);
    parser.assert_invariants();

    // Verify the sink received all printable characters
    let printed: String = sink.prints.iter().collect();
    assert_eq!(printed, "Hello, World!");
}

#[test]
fn assert_invariants_after_csi_sequence() {
    let mut parser = Parser::new();
    let mut sink = RecordingSink::default();

    // SGR sequence with multiple params: bold(1), fg red(31), bg green(42)
    parser.advance(b"\x1b[1;31;42m", &mut sink);
    parser.assert_invariants();

    // Verify the CSI dispatch was recorded with correct params and final byte
    assert_eq!(
        sink.csi_dispatches.len(),
        1,
        "expected exactly one CSI dispatch"
    );
    assert_eq!(
        sink.csi_dispatches[0].0,
        vec![1, 31, 42],
        "SGR params mismatch"
    );
    assert_eq!(
        sink.csi_dispatches[0].2, b'm',
        "SGR final byte should be 'm'"
    );
}

#[test]
fn assert_invariants_after_osc_sequence() {
    let mut parser = Parser::new();
    let mut sink = RecordingSink::default();

    // Set window title (OSC 0 ; title BEL)
    parser.advance(b"\x1b]0;My Title\x07", &mut sink);
    parser.assert_invariants();

    // Verify the OSC dispatch was recorded with the title payload
    assert_eq!(
        sink.osc_dispatches.len(),
        1,
        "expected exactly one OSC dispatch"
    );
    assert_eq!(sink.osc_dispatches[0][0], b"0", "OSC command should be '0'");
    assert_eq!(
        sink.osc_dispatches[0][1], b"My Title",
        "OSC payload mismatch"
    );
}

#[test]
fn assert_invariants_after_dcs_sequence() {
    let mut parser = Parser::new();
    let mut sink = RecordingSink::default();

    // DECRQSS (DCS q $ s ST)
    parser.advance(b"\x1bPq$s\x1b\\", &mut sink);
    parser.assert_invariants();

    // Verify the DCS hook was called (final byte 'q', intermediate '$')
    assert_eq!(sink.dcs_hooks.len(), 1, "expected exactly one DCS hook");
    assert_eq!(sink.dcs_hooks[0].2, b'q', "DCS final byte should be 'q'");
    // DCS unhook should fire when ST is received
    assert_eq!(sink.dcs_unhooks, 1, "DCS should be unhooked after ST");
}

#[test]
fn assert_invariants_after_partial_sequence() {
    let mut parser = Parser::new();
    let mut sink = RecordingSink::default();

    // Start CSI but don't finish — no dispatch yet
    parser.advance(b"\x1b[1;2", &mut sink);
    parser.assert_invariants();
    assert!(
        sink.csi_dispatches.is_empty(),
        "partial CSI should not dispatch until final byte"
    );

    // Now finish it with 'm' (SGR)
    parser.advance(b"m", &mut sink);
    parser.assert_invariants();
    assert_eq!(
        sink.csi_dispatches.len(),
        1,
        "completed CSI should dispatch"
    );
    assert_eq!(
        sink.csi_dispatches[0].0,
        vec![1, 2],
        "params should be [1, 2]"
    );
    assert_eq!(sink.csi_dispatches[0].2, b'm');
}

#[test]
fn assert_invariants_after_utf8() {
    let mut parser = Parser::new();
    let mut sink = RecordingSink::default();

    // Multi-byte UTF-8 character (€ = E2 82 AC)
    parser.advance_fast("Hello € World".as_bytes(), &mut sink);
    parser.assert_invariants();

    // Verify all characters including the multi-byte € were printed
    let printed: String = sink.prints.iter().collect();
    assert_eq!(printed, "Hello € World");
}

#[test]
fn assert_invariants_after_subparams() {
    let mut parser = Parser::new();
    let mut sink = RecordingSink::default();

    // Underline style with subparam (4:3 = curly underline)
    parser.advance(b"\x1b[4:3m", &mut sink);
    parser.assert_invariants();

    // Verify the subparam dispatch was recorded
    assert_eq!(
        sink.csi_dispatches_with_subparams.len(),
        1,
        "subparam CSI should trigger csi_dispatch_with_subparams"
    );
    assert_eq!(sink.csi_dispatches_with_subparams[0].2, b'm');
}

#[test]
fn assert_invariants_after_max_params() {
    let mut parser = Parser::new();
    let mut sink = RecordingSink::default();

    // Sequence with 16 parameters
    parser.advance(b"\x1b[1;2;3;4;5;6;7;8;9;10;11;12;13;14;15;16m", &mut sink);
    parser.assert_invariants();

    // Verify all 16 params were captured in the dispatch
    assert_eq!(sink.csi_dispatches.len(), 1, "expected one CSI dispatch");
    assert_eq!(
        sink.csi_dispatches[0].0.len(),
        16,
        "all 16 parameters should be captured"
    );
    assert_eq!(sink.csi_dispatches[0].0[0], 1);
    assert_eq!(sink.csi_dispatches[0].0[15], 16);
}

#[test]
fn assert_invariants_after_reset() {
    let mut parser = Parser::new();
    let mut sink = RecordingSink::default();

    // Put parser in various states
    parser.advance(b"\x1b[1;2;3m", &mut sink);
    parser.advance(b"\x1b]0;title", &mut sink); // Partial OSC

    parser.reset();
    parser.assert_invariants();

    // After reset, should be back to ground state
    assert_eq!(parser.state(), State::Ground);
}

#[test]
fn assert_invariants_apc_sequence() {
    let mut parser = Parser::new();
    let mut sink = RecordingSink::default();

    // APC sequence (ESC _ ... ST)
    parser.advance(b"\x1b_application data\x1b\\", &mut sink);
    parser.assert_invariants();

    // APC should have been dispatched: one start, data bytes, one end
    assert_eq!(sink.apc_starts, 1, "APC start should fire once");
    assert_eq!(sink.apc_ends, 1, "APC end should fire once");
    assert_eq!(
        sink.apc_data,
        b"application data".to_vec(),
        "APC payload should match the data between ESC_ and ST"
    );
    // Parser should return to Ground after ST
    assert_eq!(parser.state(), State::Ground);
}

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
/// prefixes every two bytes more. Each path is driven from the states it
/// reaches itself: the fast path (`advance_fast`, the one every terminal runs)
/// and the byte path (`advance`), which collects no UTF-8 and so never holds a
/// pending character (it skips the `Ground` starts that are one: their bytes
/// are tails of the starts it does sweep). Two transitions are pinned on the
/// way: CAN (0x18) and SUB (0x1A) end every sequence at `Ground`, and with C1
/// controls off no byte in 0x80..=0x9F moves a `Ground` parser.
#[test]
fn every_state_every_byte_keeps_the_type_invariant() {
    let starts = reachable_starts();
    for c1 in [false, true] {
        for fast in [false, true] {
            let feed = |parser: &mut Parser, bytes: &[u8]| {
                if fast {
                    parser.advance_fast(bytes, &mut NullSink);
                } else {
                    parser.advance(bytes, &mut NullSink);
                }
            };
            for (prefix, state) in &starts {
                if !fast && *state == State::Ground && !prefix.is_empty() {
                    continue;
                }
                let mut start = if c1 {
                    Parser::with_c1_controls()
                } else {
                    Parser::new()
                };
                feed(&mut start, prefix);
                let at = || format!("c1={c1} fast={fast} {prefix:02x?}");
                assert_eq!(start.state(), *state, "{} reaches {state:?}", at());
                if *state == State::Ground && crate::utf8::Utf8Prefix::from_bytes(prefix).is_some()
                {
                    assert_eq!(usize::from(start.utf8_len), prefix.len(), "{} pends", at());
                }
                assert_invariants_at(&start, at);
                for first in 0..=u8::MAX {
                    let mut one = start.clone();
                    feed(&mut one, &[first]);
                    let at = || format!("c1={c1} fast={fast} {prefix:02x?} + {first:02x}");
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
                            format!("c1={c1} fast={fast} {prefix:02x?} + {first:02x} {second:02x}")
                        });
                    }
                }
            }
        }
    }
}
