// Copyright 2026 Andrew Yates
// Author: Andrew Yates
// SPDX-License-Identifier: Apache-2.0

//! Basic parser tests: plain text, control characters, ESC, OSC, DCS sequences,
//! and state transition behavior.

use super::super::*;
use super::RecordingSink;

// ============== Basic Tests ==============

#[test]
fn parse_plain_text() {
    let mut parser = Parser::new();
    let mut sink = RecordingSink::default();

    parser.advance(b"Hello", &mut sink);

    assert_eq!(sink.prints.len(), 5);
    assert_eq!(sink.prints, vec!['H', 'e', 'l', 'l', 'o']);
}

#[test]
fn parse_control_character() {
    let mut parser = Parser::new();
    let mut sink = RecordingSink::default();

    parser.advance(b"\n\r\t", &mut sink);

    assert_eq!(sink.executes, vec![b'\n', b'\r', b'\t']);
}

// ============== ESC Tests ==============

#[test]
fn parse_esc_sequence() {
    let mut parser = Parser::new();
    let mut sink = RecordingSink::default();

    // ESC 7 (save cursor)
    parser.advance(b"\x1b7", &mut sink);

    assert_eq!(sink.esc_dispatches.len(), 1);
    assert_eq!(sink.esc_dispatches[0], (vec![], b'7'));
}

#[test]
fn parse_esc_with_intermediate() {
    let mut parser = Parser::new();
    let mut sink = RecordingSink::default();

    // ESC ( B (set G0 to ASCII)
    parser.advance(b"\x1b(B", &mut sink);

    assert_eq!(sink.esc_dispatches.len(), 1);
    assert_eq!(sink.esc_dispatches[0], (vec![b'('], b'B'));
}

// ============== OSC Tests ==============

#[test]
fn parse_osc_with_bel() {
    let mut parser = Parser::new();
    let mut sink = RecordingSink::default();

    // OSC 0 ; title BEL (set window title)
    parser.advance(b"\x1b]0;My Title\x07", &mut sink);

    assert_eq!(sink.osc_dispatches.len(), 1);
    assert_eq!(
        sink.osc_dispatches[0],
        vec![b"0".to_vec(), b"My Title".to_vec()]
    );
}

#[test]
fn parse_osc_c1_st_is_data_not_terminator() {
    let mut parser = Parser::new();
    let mut sink = RecordingSink::default();

    // 0x9C is treated as data in OscString state (not ST terminator).
    // This is required for UTF-8 correctness: 0x9C is a valid continuation
    // byte in CJK characters (e.g., 本 = E6 9C AC). Part of #3745 fix.
    // Terminate with BEL to flush the OSC.
    parser.advance(b"\x1b]0;Title\x9cMore\x07", &mut sink);

    assert_eq!(sink.osc_dispatches.len(), 1);
    // 0x9C is accumulated as data in the payload
    let expected_payload: Vec<u8> = b"Title\x9cMore".to_vec();
    assert_eq!(
        sink.osc_dispatches[0],
        vec![b"0".to_vec(), expected_payload],
        "0x9C should be data, not ST terminator, in OscString state"
    );
}

#[test]
fn parse_osc_with_esc_backslash() {
    let mut parser = Parser::new();
    let mut sink = RecordingSink::default();

    // OSC 0 ; title ESC \ (string terminator)
    parser.advance(b"\x1b]0;Title\x1b\\", &mut sink);

    assert_eq!(sink.osc_dispatches.len(), 1);
    assert_eq!(
        sink.osc_dispatches[0],
        vec![b"0".to_vec(), b"Title".to_vec()],
        "ESC-backslash terminator should produce same parsed payload as BEL"
    );
}

// ============== DCS Tests ==============

#[test]
fn parse_dcs_sequence() {
    let mut parser = Parser::new();
    let mut sink = RecordingSink::default();

    // DCS with params, data, and ST terminator
    parser.advance(b"\x1bP1$qm\x1b\\", &mut sink);

    assert_eq!(sink.dcs_hooks.len(), 1);
    assert_eq!(sink.dcs_unhooks, 1);
}

#[test]
fn parse_dcs_passthrough() {
    let mut parser = Parser::new();
    let mut sink = RecordingSink::default();

    // DCS q data ST (Sixel graphics)
    parser.advance(b"\x1bPqABC\x1b\\", &mut sink);

    assert_eq!(sink.dcs_hooks.len(), 1);
    assert_eq!(sink.dcs_puts, vec![b'A', b'B', b'C']);
    assert_eq!(sink.dcs_unhooks, 1);
}

// ============== State Transition Tests ==============

#[test]
fn cancel_aborts_sequence() {
    let mut parser = Parser::new();
    let mut sink = RecordingSink::default();

    // Start CSI sequence, then CAN (0x18) aborts it
    parser.advance(b"\x1b[31\x18Hello", &mut sink);

    // CAN should be executed
    assert!(sink.executes.contains(&0x18));
    // No CSI dispatch
    assert_eq!(sink.csi_dispatches.len(), 0);
    // "Hello" should be printed
    assert_eq!(sink.prints.len(), 5);
}

#[test]
fn esc_interrupts_sequence() {
    let mut parser = Parser::new();
    let mut sink = RecordingSink::default();

    // Start CSI sequence, then ESC starts new sequence
    parser.advance(b"\x1b[31\x1b[32m", &mut sink);

    // Only the second CSI should complete
    assert_eq!(sink.csi_dispatches.len(), 1);
    assert_eq!(sink.csi_dispatches[0].0, vec![32]);
}

#[test]
fn reset_clears_state() {
    let mut parser = Parser::new();
    let mut sink = RecordingSink::default();

    // Parse partial sequence
    parser.advance(b"\x1b[31", &mut sink);
    assert_eq!(parser.state(), State::CsiParam);

    parser.reset();

    assert_eq!(parser.state(), State::Ground);
    // Parse new sequence
    parser.advance(b"\x1b[32m", &mut sink);
    assert_eq!(sink.csi_dispatches.len(), 1);
    assert_eq!(sink.csi_dispatches[0].0, vec![32]);
}

// ============== Latin-1 byte handling tests (#7160) ==============

/// Bytes 0xA0-0xFF in Ground state should be printed as Latin-1 characters
/// via the basic advance() path, not silently dropped.
#[test]
fn advance_latin1_bytes_printed_not_dropped() {
    let mut parser = Parser::new();
    let mut sink = RecordingSink::default();

    // 0xE9 = Latin-1 'e with acute' (U+00E9 = 'é')
    // 0xFC = Latin-1 'u with diaeresis' (U+00FC = 'ü')
    // 0xA0 = non-breaking space (U+00A0)
    // 0xFF = Latin-1 'y with diaeresis' (U+00FF = 'ÿ')
    parser.advance(&[0xE9, 0xFC, 0xA0, 0xFF], &mut sink);

    assert_eq!(
        sink.prints.len(),
        4,
        "all 4 Latin-1 bytes should produce print actions"
    );
    assert_eq!(sink.prints[0], '\u{00E9}'); // é
    assert_eq!(sink.prints[1], '\u{00FC}'); // ü
    assert_eq!(sink.prints[2], '\u{00A0}'); // non-breaking space
    assert_eq!(sink.prints[3], '\u{00FF}'); // ÿ
}

/// Latin-1 range bytes should only be treated as printable in Ground state
/// via advance(). In other states (e.g. inside an escape sequence), they
/// go through the normal table transitions.
#[test]
fn advance_latin1_bytes_only_in_ground_state() {
    let mut parser = Parser::new();
    let mut sink = RecordingSink::default();

    // In Ground state, 0xE9 should print
    parser.advance(&[0xE9], &mut sink);
    assert_eq!(sink.prints.len(), 1);
    assert_eq!(sink.prints[0], '\u{00E9}');
}

/// C1 control bytes (0x80-0x9F) should still be handled as replacement
/// characters when C1 controls are disabled, not as Latin-1 printable chars.
#[test]
fn advance_c1_bytes_still_replacement_chars() {
    let mut parser = Parser::new();
    let mut sink = RecordingSink::default();

    // 0x85 = NEL in C1 range, should become replacement character
    parser.advance(&[0x85], &mut sink);
    assert_eq!(sink.prints.len(), 1);
    assert_eq!(sink.prints[0], char::REPLACEMENT_CHARACTER);
}

/// The advance_fast() path treats high bytes as UTF-8 (by design),
/// so isolated Latin-1 bytes like 0xE9 become replacement characters
/// since they're incomplete UTF-8 sequences. The basic advance() path
/// handles them as Latin-1 instead. This test verifies the distinction.
#[test]
fn advance_fast_treats_high_bytes_as_utf8() {
    let mut parser = Parser::new();
    let mut sink = RecordingSink::default();

    // 0xE9 is a UTF-8 lead byte (expects 2 more continuation bytes).
    // Followed by ASCII 'W', the UTF-8 decode fails → replacement character.
    // This is correct: advance_fast assumes UTF-8 input.
    parser.advance_fast(&[0xE9, b'W'], &mut sink);

    // First print should be replacement character (incomplete UTF-8)
    assert_eq!(
        sink.prints[0],
        char::REPLACEMENT_CHARACTER,
        "advance_fast should treat 0xE9 as UTF-8 lead byte, not Latin-1"
    );
}
