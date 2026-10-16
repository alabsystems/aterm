// Copyright 2026 Andrew Yates
// Author: Andrew Yates
// SPDX-License-Identifier: Apache-2.0

//! String-state bulk paths (OSC/DCS): the fast path's bulk copies agree with
//! the per-byte state machine.

use super::super::*;
use super::RecordingSink;

/// In `OscString` the fast path appends `osc_bulk_copy_len(len, n)` of the `n`
/// data bytes before the first control byte; the per-byte path's `OscPut`
/// pushes data byte `i` onto a buffer of `L(i) = min(len + i, MAX)` bytes only
/// while `osc_put_keeps(L(i))`. At every breakpoint of the two piecewise-linear
/// functions — `n` and `i` at 0, 1 and next to `n - 1` and `room = MAX - len`
/// — byte `i` is pushed exactly when the bulk copy keeps it, `OscPut` takes
/// `L(i)` to `L(i + 1)`, both paths end the run at the same length, and that
/// length is within the cap. Placed relative to `room`, those points are every
/// branch either function has at a given `len`, so `len` runs over both ends
/// of the buffer in full and a prime stride between (every `len` would take
/// seconds and reach no other branch).
#[test]
fn osc_bulk_copy_agrees_with_per_byte_puts_at_every_breakpoint() {
    use crate::dispatch::{osc_bulk_copy_len, osc_put_keeps};
    let ends = 4096;
    let lens = (0..=ends)
        .chain((ends..MAX_OSC_DATA - ends).step_by(4093))
        .chain(MAX_OSC_DATA - ends..=MAX_OSC_DATA);
    for len in lens {
        let room = MAX_OSC_DATA - len;
        for n in [
            0,
            1,
            2,
            room.saturating_sub(1),
            room,
            room + 1,
            room + 2,
            MAX_OSC_DATA + 1,
        ] {
            let copy_len = osc_bulk_copy_len(len, n);
            assert_eq!(len + copy_len, (len + n).min(MAX_OSC_DATA), "{len} {n}");
            for i in [
                0,
                1,
                n.saturating_sub(2),
                n.saturating_sub(1),
                room.saturating_sub(1),
                room,
                room + 1,
            ] {
                if i >= n {
                    continue;
                }
                let before = (len + i).min(MAX_OSC_DATA);
                let pushed = osc_put_keeps(before);
                assert_eq!(
                    (len + i + 1).min(MAX_OSC_DATA),
                    before + usize::from(pushed),
                    "{len} {n} {i}"
                );
                assert_eq!(pushed, i < copy_len, "{len} {n} {i}");
            }
        }
    }
}

#[test]
fn osc_sub_cap_payload_delivered_intact() {
    // A payload comfortably under the cap must be delivered without truncation,
    // identically by the bulk and byte-by-byte paths.
    let payload_len = MAX_OSC_DATA - 1024;

    let mut input = vec![b'\x1b', b']', b'0', b';'];
    input.extend(std::iter::repeat_n(b'X', payload_len));
    input.push(0x07); // BEL terminator

    let mut parser_slow = Parser::new();
    let mut sink_slow = RecordingSink::default();
    parser_slow.advance(&input, &mut sink_slow);

    let mut parser_fast = Parser::new();
    let mut sink_fast = RecordingSink::default();
    parser_fast.advance_fast(&input, &mut sink_fast);

    assert_eq!(
        sink_slow.osc_dispatches, sink_fast.osc_dispatches,
        "bulk and byte-by-byte OSC paths must agree below the cap"
    );
    assert_eq!(sink_fast.osc_dispatches.len(), 1);
    // params == ["0", payload]; the payload segment must be intact.
    assert_eq!(
        sink_fast.osc_dispatches[0][1].len(),
        payload_len,
        "sub-cap payload must not be truncated"
    );
}

#[test]
fn osc_over_capacity_truncation_parity() {
    // A payload that EXCEEDS MAX_OSC_DATA must be truncated at the cap, and the
    // bulk fast path and the byte-by-byte slow path must truncate identically.
    let over = MAX_OSC_DATA + 50_000;

    let mut input = vec![b'\x1b', b']', b'0', b';'];
    input.extend(std::iter::repeat_n(b'Y', over)); // well over the cap
    input.push(0x07);

    let mut parser_slow = Parser::new();
    let mut sink_slow = RecordingSink::default();
    parser_slow.advance(&input, &mut sink_slow);

    let mut parser_fast = Parser::new();
    let mut sink_fast = RecordingSink::default();
    parser_fast.advance_fast(&input, &mut sink_fast);

    assert_eq!(
        sink_slow.osc_dispatches, sink_fast.osc_dispatches,
        "bulk and byte-by-byte OSC paths must truncate identically at the cap"
    );
    assert_eq!(sink_fast.osc_dispatches.len(), 1);
    // osc_data holds "0;" + payload, total capped at MAX_OSC_DATA bytes. The
    // "0;" prefix costs 2 bytes, leaving MAX_OSC_DATA - 2 for the payload.
    assert_eq!(
        sink_fast.osc_dispatches[0][1].len(),
        MAX_OSC_DATA - 2,
        "over-cap payload must be truncated at the MAX_OSC_DATA boundary"
    );
}

#[test]
fn osc_at_exact_cap_boundary_parity() {
    // Drive osc_data to exactly MAX_OSC_DATA, then a single byte over, in two
    // calls, to exercise the `copy_len < n` (buffer fills before C0) branch of
    // the fast path against the slow path.
    let prefix_len = 2; // "0;"
    let exact_payload = MAX_OSC_DATA - prefix_len; // fills the buffer exactly

    let mut input = vec![b'\x1b', b']', b'0', b';'];
    input.extend(std::iter::repeat_n(b'Z', exact_payload + 1)); // one over
    input.push(0x07);

    let mut parser_slow = Parser::new();
    let mut sink_slow = RecordingSink::default();
    parser_slow.advance(&input, &mut sink_slow);

    let mut parser_fast = Parser::new();
    let mut sink_fast = RecordingSink::default();
    parser_fast.advance_fast(&input, &mut sink_fast);

    assert_eq!(
        sink_slow.osc_dispatches, sink_fast.osc_dispatches,
        "paths must agree at the exact capacity boundary"
    );
    assert_eq!(
        sink_fast.osc_dispatches[0][1].len(),
        exact_payload,
        "payload must be truncated to exactly fill the buffer"
    );
}

// --- DCS passthrough bulk fast-path vs. byte-by-byte equivalence for 0x7F ---
//
// In DcsPassthrough the transition table maps 0x7F (DEL) to ActionType::Ignore
// (stays in the state, emits NOTHING). The bulk fast path in dispatch.rs must
// therefore stop bulk-copying at 0x7F (via find_dcs_passthrough_boundary) and
// let the byte-by-byte path drop it — otherwise advance_fast would leak DEL into
// the DCS sink while advance() strips it. These tests pin that parity so the
// fast path can never re-diverge. NOTE: this is DCS-specific; APC legitimately
// emits 0x7F as ApcPut data and is intentionally unaffected.

#[test]
fn dcs_passthrough_del_parity_advance_vs_advance_fast() {
    // ESC P q A 0x7F B ESC backslash — the canonical failing scenario. The
    // byte-by-byte path Ignores 0x7F, so the DCS sink sees [A, B]; the bulk
    // fast path must produce the identical output.
    let input: &[u8] = &[0x1B, b'P', b'q', b'A', 0x7F, b'B', 0x1B, b'\\'];

    let mut parser_slow = Parser::new();
    let mut sink_slow = RecordingSink::default();
    parser_slow.advance(input, &mut sink_slow);

    let mut parser_fast = Parser::new();
    let mut sink_fast = RecordingSink::default();
    parser_fast.advance_fast(input, &mut sink_fast);

    assert_eq!(
        sink_slow.dcs_puts, sink_fast.dcs_puts,
        "DCS bulk fast path must Ignore 0x7F identically to byte-by-byte"
    );
    assert_eq!(
        sink_fast.dcs_puts,
        vec![b'A', b'B'],
        "0x7F (DEL) must be stripped from DCS passthrough, not leaked to the sink"
    );
    assert_eq!(
        sink_slow.dcs_unhooks, sink_fast.dcs_unhooks,
        "both paths must unhook the DCS once"
    );
    assert_eq!(parser_slow.state as u8, parser_fast.state as u8);
}

#[test]
fn dcs_passthrough_del_positions_parity() {
    // DEL at start, multiple consecutive DELs, and DEL adjacent to terminator.
    // Each must yield identical DCS sink output across both paths.
    let cases: &[&[u8]] = &[
        &[0x1B, b'P', b'q', 0x7F, b'A', 0x1B, b'\\'], // DEL at start -> [A]
        &[0x1B, b'P', b'q', 0x7F, 0x7F, b'X', 0x1B, b'\\'], // multiple DEL -> [X]
        &[0x1B, b'P', b'q', b'A', 0x7F, 0x1B, b'\\'], // DEL before ST -> [A]
        &[0x1B, b'P', b'q', b'A', 0x07, b'B', 0x1B, b'\\'], // BEL is genuine data -> [A,7,B]
    ];

    for input in cases {
        let mut parser_slow = Parser::new();
        let mut sink_slow = RecordingSink::default();
        parser_slow.advance(input, &mut sink_slow);

        let mut parser_fast = Parser::new();
        let mut sink_fast = RecordingSink::default();
        parser_fast.advance_fast(input, &mut sink_fast);

        assert_eq!(
            sink_slow.dcs_puts, sink_fast.dcs_puts,
            "DCS dcs_puts mismatch between advance and advance_fast for input: {input:?}"
        );
        assert_eq!(
            sink_slow.dcs_unhooks, sink_fast.dcs_unhooks,
            "DCS dcs_unhooks mismatch for input: {input:?}"
        );
    }
}
