// Copyright 2026 Andrew Yates
// Author: Andrew Yates
// SPDX-License-Identifier: Apache-2.0

//! Plain text, controls, ESC, OSC and DCS sequences, and state transitions.

use super::super::*;
use super::RecordingSink;

#[test]
fn parse_plain_text() {
    let mut parser = Parser::new();
    let mut sink = RecordingSink::default();

    parser.advance_fast(b"Hello", &mut sink);

    assert_eq!(sink.prints.len(), 5);
    assert_eq!(sink.prints, vec!['H', 'e', 'l', 'l', 'o']);
}

#[test]
fn parse_control_character() {
    let mut parser = Parser::new();
    let mut sink = RecordingSink::default();

    parser.advance_fast(b"\n\r\t", &mut sink);

    assert_eq!(sink.executes, vec![b'\n', b'\r', b'\t']);
}

#[test]
fn parse_esc_sequence() {
    let mut parser = Parser::new();
    let mut sink = RecordingSink::default();

    parser.advance_fast(b"\x1b7", &mut sink);

    assert_eq!(sink.esc_dispatches.len(), 1);
    assert_eq!(sink.esc_dispatches[0], (vec![], b'7'));
}

#[test]
fn parse_esc_with_intermediate() {
    let mut parser = Parser::new();
    let mut sink = RecordingSink::default();

    parser.advance_fast(b"\x1b(B", &mut sink);

    assert_eq!(sink.esc_dispatches.len(), 1);
    assert_eq!(sink.esc_dispatches[0], (vec![b'('], b'B'));
}

#[test]
fn parse_osc_with_bel() {
    let mut parser = Parser::new();
    let mut sink = RecordingSink::default();

    parser.advance_fast(b"\x1b]0;My Title\x07", &mut sink);

    assert_eq!(sink.osc_dispatches.len(), 1);
    assert_eq!(
        sink.osc_dispatches[0],
        vec![b"0".to_vec(), b"My Title".to_vec()]
    );
}

#[test]
fn parse_osc_with_esc_backslash() {
    let mut parser = Parser::new();
    let mut sink = RecordingSink::default();

    parser.advance_fast(b"\x1b]0;Title\x1b\\", &mut sink);

    assert_eq!(sink.osc_dispatches.len(), 1);
    assert_eq!(
        sink.osc_dispatches[0],
        vec![b"0".to_vec(), b"Title".to_vec()],
        "ESC-backslash terminator should produce same parsed payload as BEL"
    );
}

#[test]
fn parse_dcs_passthrough() {
    let mut parser = Parser::new();
    let mut sink = RecordingSink::default();

    parser.advance_fast(b"\x1bPqABC\x1b\\", &mut sink);

    assert_eq!(sink.dcs_hooks.len(), 1);
    assert_eq!(sink.dcs_puts, vec![b'A', b'B', b'C']);
    assert_eq!(sink.dcs_unhooks, 1);
}

#[test]
fn cancel_aborts_sequence() {
    let mut parser = Parser::new();
    let mut sink = RecordingSink::default();

    parser.advance_fast(b"\x1b[31\x18Hello", &mut sink);

    assert!(sink.executes.contains(&0x18));
    assert_eq!(sink.csi_dispatches.len(), 0);
    assert_eq!(sink.prints.len(), 5);
}

#[test]
fn esc_interrupts_sequence() {
    let mut parser = Parser::new();
    let mut sink = RecordingSink::default();

    parser.advance_fast(b"\x1b[31\x1b[32m", &mut sink);

    assert_eq!(sink.csi_dispatches.len(), 1);
    assert_eq!(sink.csi_dispatches[0].0, vec![32]);
}

#[test]
fn reset_clears_state() {
    let mut parser = Parser::new();
    let mut sink = RecordingSink::default();

    parser.advance_fast(b"\x1b[31", &mut sink);
    assert_eq!(parser.state(), State::CsiParam);

    parser.reset();

    assert_eq!(parser.state(), State::Ground);
    parser.advance_fast(b"\x1b[32m", &mut sink);
    assert_eq!(sink.csi_dispatches.len(), 1);
    assert_eq!(sink.csi_dispatches[0].0, vec![32]);
}

/// Every introducer ends where its terminator says, in both C1 modes:
/// `ESC [` with every digit and every final byte (0x40..=0x7E), `ESC ]` with
/// every printable data byte and BEL, and `ESC P q` with ST all leave the
/// parser at `Ground`.
#[test]
fn every_introducer_terminates() {
    let mut streams: Vec<Vec<u8>> = Vec::new();
    for digit in b'0'..=b'9' {
        for final_byte in 0x40..=0x7E {
            streams.push(vec![0x1B, b'[', digit, final_byte]);
        }
    }
    for data in 0x20..=0x7E {
        streams.push(vec![0x1B, b']', data, 0x07]);
    }
    streams.push(b"\x1bPq\x1b\\".to_vec());
    for c1 in [false, true] {
        for stream in &streams {
            let mut parser = if c1 {
                Parser::with_c1_controls()
            } else {
                Parser::new()
            };
            parser.advance_fast(stream, &mut NullSink);
            assert_eq!(parser.state(), State::Ground, "c1={c1} {stream:02x?}");
        }
    }
}
