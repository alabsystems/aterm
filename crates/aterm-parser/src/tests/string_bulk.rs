// Copyright 2026 Andrew Yates
// Author: Andrew Yates
// SPDX-License-Identifier: Apache-2.0

//! String-state bulk paths (OSC/DCS): bulk copies agree with the per-byte state machine.

use super::super::*;
use super::RecordingSink;

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
fn osc_payload_cap_parity() {
    for (payload_len, delivered) in [
        (MAX_OSC_DATA - 1024, MAX_OSC_DATA - 1024),
        (MAX_OSC_DATA - 1, MAX_OSC_DATA - 2),
        (MAX_OSC_DATA + 50_000, MAX_OSC_DATA - 2),
    ] {
        let mut input = vec![b'\x1b', b']', b'0', b';'];
        input.extend(std::iter::repeat_n(b'X', payload_len));
        input.push(0x07); // BEL terminator

        let mut sink_slow = RecordingSink::default();
        let mut per_byte = Parser::new();
        let (head, tail) = input.split_at(MAX_OSC_DATA - 4096);
        per_byte.advance_fast(head, &mut sink_slow);
        for byte in tail {
            per_byte.advance_fast(std::slice::from_ref(byte), &mut sink_slow);
        }
        let mut sink_fast = RecordingSink::default();
        Parser::new().advance_fast(&input, &mut sink_fast);

        assert_eq!(
            sink_slow.osc_dispatches, sink_fast.osc_dispatches,
            "payload {payload_len}: bulk and byte-by-byte OSC paths must agree"
        );
        assert_eq!(sink_fast.osc_dispatches.len(), 1);
        assert_eq!(
            sink_fast.osc_dispatches[0][1].len(),
            delivered,
            "payload {payload_len}"
        );
    }
}

#[test]
fn dcs_passthrough_del_parity_whole_vs_per_byte() {
    let cases: &[(&[u8], &[u8])] = &[
        (&[0x1B, b'P', b'q', b'A', 0x7F, b'B', 0x1B, b'\\'], b"AB"),
        (&[0x1B, b'P', b'q', 0x7F, b'A', 0x1B, b'\\'], b"A"),
        (&[0x1B, b'P', b'q', 0x7F, 0x7F, b'X', 0x1B, b'\\'], b"X"),
        (&[0x1B, b'P', b'q', b'A', 0x7F, 0x1B, b'\\'], b"A"),
        (
            &[0x1B, b'P', b'q', b'A', 0x07, b'B', 0x1B, b'\\'],
            b"A\x07B",
        ),
    ];
    for &(input, puts) in cases {
        let mut parser_slow = Parser::new();
        let mut sink_slow = RecordingSink::default();
        for byte in input {
            parser_slow.advance_fast(std::slice::from_ref(byte), &mut sink_slow);
        }

        let mut parser_fast = Parser::new();
        let mut sink_fast = RecordingSink::default();
        parser_fast.advance_fast(input, &mut sink_fast);

        assert_eq!(sink_slow.dcs_puts, puts, "{input:02x?}");
        assert_eq!(sink_fast.dcs_puts, puts, "{input:02x?}");
        assert_eq!(
            (sink_slow.dcs_unhooks, sink_fast.dcs_unhooks),
            (1, 1),
            "{input:02x?}"
        );
        assert_eq!(parser_slow.state as u8, parser_fast.state as u8);
    }
}
