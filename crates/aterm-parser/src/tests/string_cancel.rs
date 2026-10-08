// Copyright 2026 Andrew Yates
// Author: Andrew Yates
// SPDX-License-Identifier: Apache-2.0

//! CAN (0x18) and SUB (0x1A) cancel a control string: an OSC, SOS, PM or APC
//! string they interrupt ends undelivered, as xterm ends it (its string table
//! sends both to the ground state without dispatching) and as a DCS here
//! already ends (`dcs_unhook(canceled = true)`). A truncated OSC 52 clipboard
//! write, OSC 8 link or title is never dispatched.

use super::*;

/// What a parser fed `bytes` in the chunks `cuts` names (bit `k` set: a chunk
/// ends after byte `k`) delivered.
fn feed_chunked(parser: &mut Parser, bytes: &[u8], cuts: u32) -> RecordingSink {
    let mut sink = RecordingSink::default();
    let mut start = 0;
    for end in 1..=bytes.len() {
        if end == bytes.len() || cuts & (1 << (end - 1)) != 0 {
            parser.advance_fast(&bytes[start..end], &mut sink);
            start = end;
        }
    }
    sink
}

fn parser(c1: bool) -> Parser {
    let mut parser = Parser::new();
    parser.c1_controls_enabled = c1;
    parser
}

/// An OSC interrupted by CAN or SUB dispatches nothing, in every chunking
/// and both C1 modes; the cancel byte executes and the text after it prints.
#[test]
fn can_and_sub_cancel_an_osc_string() {
    for osc in [&b"52;c;aGk="[..], b"8;;a:b", b"2;title"] {
        for cancel in [0x18u8, 0x1A] {
            let mut bytes = b"\x1b]".to_vec();
            bytes.extend_from_slice(osc);
            bytes.push(cancel);
            bytes.extend_from_slice(b"ok");
            let shown = String::from_utf8_lossy(&bytes).into_owned();
            for c1 in [false, true] {
                for cuts in 0..1u32 << (bytes.len() - 1) {
                    let sink = feed_chunked(&mut parser(c1), &bytes, cuts);
                    assert!(
                        sink.osc_dispatches.is_empty(),
                        "{shown:?} c1={c1} cuts {cuts:#b}: a canceled OSC dispatched {:?}",
                        sink.osc_dispatches
                    );
                    assert_eq!(sink.executes, vec![cancel], "{shown:?} cuts {cuts:#b}");
                    assert_eq!(sink.prints.iter().collect::<String>(), "ok");
                }
            }
        }
    }
}

/// Negative control: BEL, `ESC \` and (with C1 on) the 8-bit ST still end an
/// OSC by dispatching it, and a cancel leaves the next OSC whole.
#[test]
fn a_terminated_osc_still_dispatches() {
    for (c1, bytes) in [
        (false, &b"\x1b]2;title\x07ok"[..]),
        (false, b"\x1b]2;title\x1b\\ok"),
        (true, b"\x1b]2;title\x9cok"),
    ] {
        for cuts in 0..1u32 << (bytes.len() - 1) {
            let sink = feed_chunked(&mut parser(c1), bytes, cuts);
            assert_eq!(
                sink.osc_dispatches,
                vec![vec![b"2".to_vec(), b"title".to_vec()]],
                "{:?} cuts {cuts:#b}",
                String::from_utf8_lossy(bytes)
            );
        }
    }
    let sink = feed_chunked(&mut Parser::new(), b"\x1b]2;cut\x18\x1b]2;whole\x07", 0);
    assert_eq!(
        sink.osc_dispatches,
        vec![vec![b"2".to_vec(), b"whole".to_vec()]]
    );
}

/// An APC (a kitty graphics command) interrupted by CAN or SUB is not ended
/// as complete; an SOS or PM string, which has no consumer, is swallowed the
/// same way. Negative control: ST still ends the APC.
#[test]
fn can_and_sub_cancel_an_apc_string() {
    for cancel in [0x18u8, 0x1A] {
        let mut bytes = b"\x1b_Ga=T;AAAA".to_vec();
        bytes.push(cancel);
        bytes.extend_from_slice(b"ok");
        for c1 in [false, true] {
            for cuts in 0..1u32 << (bytes.len() - 1) {
                let sink = feed_chunked(&mut parser(c1), &bytes, cuts);
                assert_eq!(
                    (sink.apc_starts, sink.apc_ends, sink.apc_cancels),
                    (1, 0, 1),
                    "cancel {cancel:#04x} c1={c1} cuts {cuts:#b}: a canceled APC ended as complete"
                );
                assert_eq!(sink.prints.iter().collect::<String>(), "ok");
            }
        }
        for intro in [&b"\x1bX"[..], b"\x1b^"] {
            let mut bytes = intro.to_vec();
            bytes.extend_from_slice(b"payload");
            bytes.push(cancel);
            bytes.extend_from_slice(b"ok");
            let sink = feed_chunked(&mut Parser::new(), &bytes, 0);
            assert_eq!(
                (sink.apc_starts, sink.apc_ends, sink.apc_cancels),
                (0, 0, 0)
            );
            assert_eq!(sink.prints.iter().collect::<String>(), "ok");
        }
    }
    let sink = feed_chunked(&mut Parser::new(), b"\x1b_Ga=q;\x1b\\ok", 0);
    assert_eq!(
        (sink.apc_starts, sink.apc_ends, sink.apc_cancels),
        (1, 1, 0)
    );
}

/// A canceled OSC is canceled across a seamless update too: the successor
/// restores the carried head, and a CAN in the tail ends it undelivered —
/// both for a carried payload and for one carried as over the cap.
#[test]
fn a_carried_osc_is_canceled_by_can_in_the_tail() {
    let mut outgoing = Parser::new();
    outgoing.advance_fast(b"\x1b]52;c;aGVs", &mut RecordingSink::default());
    let carry = outgoing.carry().expect("carried");
    let mut over_cap = carry.clone();
    over_cap.osc.clear();
    over_cap.osc_overflowed = true;
    for carry in [carry, over_cap] {
        let mut successor = Parser::new();
        successor.restore_carry(&carry).expect("restores");
        let sink = feed_chunked(&mut successor, b"bG8=\x18\x1b]2;next\x07", 0);
        assert_eq!(
            sink.osc_dispatches,
            vec![vec![b"2".to_vec(), b"next".to_vec()]],
            "{carry:?}: only the OSC after the cancel dispatches"
        );
    }
}
