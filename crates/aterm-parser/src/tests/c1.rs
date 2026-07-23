// Copyright 2026 Andrew Yates
// Author: Andrew Yates
// SPDX-License-Identifier: Apache-2.0

//! C1 control code tests: 8-bit CSI/OSC introducers, default-off security
//! posture, and runtime enable/disable toggle.

use super::super::*;
use super::RecordingSink;

#[test]
fn parse_c1_csi() {
    // C1 controls require explicit opt-in (disabled by default for UTF-8 security)
    let mut parser = Parser::with_c1_controls();
    let mut sink = RecordingSink::default();

    // 8-bit CSI (0x9B) followed by params
    parser.advance(b"\x9b31m", &mut sink);

    assert_eq!(sink.csi_dispatches.len(), 1);
    assert_eq!(sink.csi_dispatches[0].0, vec![31]);
}

#[test]
fn parse_c1_osc() {
    // C1 controls require explicit opt-in (disabled by default for UTF-8 security)
    let mut parser = Parser::with_c1_controls();
    let mut sink = RecordingSink::default();

    // 8-bit OSC (0x9D) followed by data
    parser.advance(b"\x9d0;Title\x07", &mut sink);

    assert_eq!(sink.osc_dispatches.len(), 1);
    assert_eq!(
        sink.osc_dispatches[0],
        vec![b"0".to_vec(), b"Title".to_vec()],
        "C1 OSC introducer (0x9D) should produce same parsed payload as ESC ]"
    );
}

#[test]
fn osc_payload_preserves_non_st_c1_bytes() {
    let mut parser = Parser::new();
    let mut sink = RecordingSink::default();
    let payload: Vec<u8> = (0x80..=0x9B).chain(0x9D..=0x9F).collect();
    let mut input = vec![0x1B, b']', b'0', b';'];
    input.extend_from_slice(&payload);
    input.push(0x07);

    parser.advance(&input, &mut sink);

    assert_eq!(sink.osc_dispatches.len(), 1);
    assert_eq!(
        sink.osc_dispatches[0],
        vec![b"0".to_vec(), payload],
        "OSC should keep non-ST C1 bytes as payload data"
    );
    assert_eq!(parser.state(), State::Ground);
}

#[test]
fn osc_string_c1_st_byte_is_data_not_terminator() {
    let mut parser = Parser::new();
    let mut sink = RecordingSink::default();

    // 0x9C is treated as data in OscString state (not ST terminator).
    // Required for UTF-8 correctness: 0x9C is a valid continuation byte
    // in CJK characters (e.g., 本 = E6 9C AC). Terminate with BEL. (#3745)
    parser.advance(b"\x1b]0;Title\x9cX\x07", &mut sink);

    assert_eq!(sink.osc_dispatches.len(), 1);
    let expected_payload: Vec<u8> = b"Title\x9cX".to_vec();
    assert_eq!(
        sink.osc_dispatches[0],
        vec![b"0".to_vec(), expected_payload],
        "0x9C should be accumulated as data, not treated as ST terminator"
    );
    assert_eq!(parser.state(), State::Ground);
}

#[test]
fn c1_disabled_by_default() {
    // Default parser should treat C1 bytes (0x80-0x9F) as invalid UTF-8
    // This is the secure default for UTF-8 terminals
    let mut parser = Parser::new();
    let mut sink = RecordingSink::default();

    // 8-bit CSI (0x9B) should NOT be interpreted as CSI when C1 disabled
    // Instead: 0x9B -> replacement char, "31m" -> printed as regular chars
    parser.advance(b"\x9b31m", &mut sink);

    // Should NOT produce a CSI dispatch
    assert_eq!(sink.csi_dispatches.len(), 0);
    // 0x9B becomes replacement char, then "31m" are printed as chars
    // Total: 4 prints (replacement + '3' + '1' + 'm')
    assert_eq!(sink.prints.len(), 4);
    assert_eq!(sink.prints[0], char::REPLACEMENT_CHARACTER);
    assert_eq!(sink.prints[1], '3');
    assert_eq!(sink.prints[2], '1');
    assert_eq!(sink.prints[3], 'm');
}

#[test]
fn dcs_passthrough_preserves_non_st_high_bytes() {
    let mut parser = Parser::new();
    let mut sink = RecordingSink::default();

    // Build a DCS passthrough sequence containing all high bytes except 0x9C (ST).
    // ESC P @ = DCS with final byte '@' (enters DcsPassthrough)
    let payload: Vec<u8> = (0x80..=0x9Bu8).chain(0x9D..=0xFFu8).collect();
    let mut input = vec![0x1B, b'P', b'@']; // DCS hook
    input.extend_from_slice(&payload);
    input.extend_from_slice(b"\x1b\\"); // ST

    parser.advance(&input, &mut sink);

    assert_eq!(sink.dcs_hooks.len(), 1, "DCS hook should fire once");
    assert_eq!(
        sink.dcs_puts, payload,
        "DCS passthrough should keep all non-ST high bytes as payload data"
    );
    assert_eq!(sink.dcs_unhooks, 1, "DCS unhook should fire once");
    assert_eq!(parser.state(), State::Ground);
}

#[test]
fn dcs_passthrough_st_c1_byte_terminates_sequence() {
    let mut parser = Parser::new();
    let mut sink = RecordingSink::default();

    // 0x9C in DCS passthrough should act as ST (terminate the sequence)
    let mut input = vec![0x1B, b'P', b'@']; // DCS hook
    input.extend_from_slice(b"data");
    input.push(0x9C); // ST via C1
    input.push(b'X'); // should be printed as regular char

    parser.advance(&input, &mut sink);

    assert_eq!(sink.dcs_hooks.len(), 1);
    assert_eq!(sink.dcs_puts, b"data".to_vec());
    assert_eq!(sink.prints, vec!['X']);
    assert_eq!(parser.state(), State::Ground);
}

#[test]
fn dcs_passthrough_st_c1_byte_terminates_sequence_advance_fast() {
    let mut parser = Parser::new();
    let mut sink = RecordingSink::default();

    let mut input = vec![0x1B, b'P', b'@']; // DCS hook
    input.extend_from_slice(b"data");
    input.push(0x9C); // ST via C1
    input.extend_from_slice(b"VISIBLE");

    parser.advance_fast(&input, &mut sink);

    assert_eq!(sink.dcs_hooks.len(), 1);
    assert_eq!(sink.dcs_puts, b"data".to_vec());
    assert_eq!(sink.dcs_unhooks, 1, "advance_fast must unhook on 0x9C");
    assert_eq!(sink.prints, "VISIBLE".chars().collect::<Vec<_>>());
    assert_eq!(parser.state(), State::Ground);
}

#[test]
fn apc_payload_preserves_non_st_c1_bytes() {
    let mut parser = Parser::new();
    let mut sink = RecordingSink::default();

    // Build an APC sequence containing all high bytes except 0x9C (ST).
    // ESC _ = APC start
    let payload: Vec<u8> = (0x80..=0x9Bu8).chain(0x9D..=0xFFu8).collect();
    let mut input = vec![0x1B, b'_']; // APC start
    input.extend_from_slice(&payload);
    input.extend_from_slice(b"\x1b\\"); // ST

    parser.advance(&input, &mut sink);

    assert_eq!(sink.apc_starts, 1, "APC start should fire once");
    assert_eq!(
        sink.apc_data, payload,
        "APC should keep all non-ST high bytes as payload data"
    );
    assert_eq!(sink.apc_ends, 1, "APC end should fire once");
    assert_eq!(parser.state(), State::Ground);
}

#[test]
fn apc_st_c1_byte_terminates_sequence() {
    let mut parser = Parser::new();
    let mut sink = RecordingSink::default();

    // 0x9C in APC should act as ST (terminate the sequence)
    let mut input = vec![0x1B, b'_']; // APC start
    input.extend_from_slice(b"data");
    input.push(0x9C); // ST via C1
    input.push(b'X'); // should be printed as regular char

    parser.advance(&input, &mut sink);

    assert_eq!(sink.apc_starts, 1);
    assert_eq!(sink.apc_data, b"data".to_vec());
    assert_eq!(sink.prints, vec!['X']);
    assert_eq!(parser.state(), State::Ground);
}

#[test]
fn parse_c1_osc_with_c1_st() {
    // When C1 controls are enabled, 0x9C in OscString state should act as ST
    // (terminating the OSC), not be accumulated as data.
    let mut parser = Parser::with_c1_controls();
    let mut sink = RecordingSink::default();

    // C1 OSC (0x9D) + data + C1 ST (0x9C)
    parser.advance(b"\x9d0;Title\x9c", &mut sink);

    assert_eq!(sink.osc_dispatches.len(), 1);
    assert_eq!(
        sink.osc_dispatches[0],
        vec![b"0".to_vec(), b"Title".to_vec()],
        "C1 OSC with C1 ST terminator should parse correctly when C1 enabled"
    );
    assert_eq!(parser.state(), State::Ground);
}

#[test]
fn parse_c1_osc_with_c1_st_advance_fast() {
    // Same test but exercises the advance_fast (SIMD) path
    let mut parser = Parser::with_c1_controls();
    let mut sink = RecordingSink::default();

    // Build a longer payload to exercise advance_fast
    let mut input = vec![0x9D, b'0', b';'];
    input.extend_from_slice(b"A Long Window Title For Testing");
    input.push(0x9C);

    parser.advance(&input, &mut sink);

    assert_eq!(sink.osc_dispatches.len(), 1);
    assert_eq!(
        sink.osc_dispatches[0],
        vec![b"0".to_vec(), b"A Long Window Title For Testing".to_vec(),],
        "C1 OSC with C1 ST should work via advance_fast path"
    );
    assert_eq!(parser.state(), State::Ground);
}

#[test]
fn c1_can_be_enabled_at_runtime() {
    let mut parser = Parser::new();
    let mut sink = RecordingSink::default();

    // Initially disabled - C1 becomes replacement char
    parser.advance(b"\x9b", &mut sink);
    assert_eq!(sink.prints.len(), 1);
    assert_eq!(sink.csi_dispatches.len(), 0);

    // Enable C1 controls
    sink.prints.clear();
    parser.set_c1_controls_enabled(true);

    // Now C1 should be interpreted
    parser.advance(b"\x9b31m", &mut sink);
    assert_eq!(sink.csi_dispatches.len(), 1);
    assert_eq!(sink.prints.len(), 0);
}

/// Regression test for #7556: C1 control bytes must not introduce new
/// sequences when C1 controls are disabled and the parser is mid-sequence.
///
/// Before the fix, 0x9B (C1 CSI) in CsiParam state would transition to
/// CsiEntry, effectively injecting a new CSI sequence from within an
/// existing one. This could be exploited by a malicious remote server.
#[test]
fn c1_disabled_blocks_injection_in_csi_param_state() {
    let mut parser = Parser::new();
    let mut sink = RecordingSink::default();

    // Start a CSI sequence, then inject C1 CSI (0x9B) mid-params.
    // Without the fix, 0x9B would start a new CSI sequence.
    // With the fix, 0x9B is silently dropped.
    //
    // "\x1b[1" puts parser in CsiParam state, then 0x9B arrives.
    // After 0x9B is dropped, "31m" continues as params+final for the
    // original CSI, producing CSI 1;31 m (a valid SGR sequence with
    // params adjusted by the state machine).
    parser.advance(b"\x1b[1\x9b31m", &mut sink);

    // The 0x9B should NOT have started a new CSI sequence.
    // We should get exactly 1 CSI dispatch (the original \x1b[..m).
    assert_eq!(
        sink.csi_dispatches.len(),
        1,
        "C1 CSI (0x9B) in CsiParam state should be dropped when C1 disabled"
    );
}

/// Verify 0x9B in EscapeIntermediate state is dropped when C1 disabled (#7556).
#[test]
fn c1_disabled_blocks_injection_in_escape_state() {
    let mut parser = Parser::new();
    let mut sink = RecordingSink::default();

    // Put parser into Escape state with ESC, then send 0x9B.
    // Without fix: 0x9B transitions to CsiEntry (new sequence).
    // With fix: 0x9B is dropped in Escape state.
    parser.advance(b"\x1b\x9b31m", &mut sink);

    // The 0x9B should NOT have produced a CSI dispatch via C1
    assert_eq!(
        sink.csi_dispatches.len(),
        0,
        "C1 CSI (0x9B) in Escape state should be dropped when C1 disabled"
    );
}

/// Verify C1 bytes 0x90 (DCS) and 0x9D (OSC) are also blocked mid-sequence (#7556).
#[test]
fn c1_disabled_blocks_dcs_osc_injection_mid_sequence() {
    let mut parser = Parser::new();
    let mut sink = RecordingSink::default();

    // Start a CSI, inject C1 DCS (0x90) and C1 OSC (0x9D).
    // Both should be dropped, not start new DCS/OSC sequences.
    parser.advance(b"\x1b[1\x90q", &mut sink);
    assert_eq!(
        sink.dcs_hooks.len(),
        0,
        "C1 DCS (0x90) mid-CSI should be dropped when C1 disabled"
    );

    parser.advance(b"\x1b[1\x9d0;Title\x07", &mut sink);
    assert_eq!(
        sink.osc_dispatches.len(),
        0,
        "C1 OSC (0x9D) mid-CSI should be dropped when C1 disabled"
    );
}

/// When C1 controls ARE enabled, injection mid-sequence is allowed (per DEC spec).
#[test]
fn c1_enabled_allows_injection_mid_sequence() {
    let mut parser = Parser::with_c1_controls();
    let mut sink = RecordingSink::default();

    // With C1 enabled, 0x9B in CsiParam should start a new CSI (per spec).
    parser.advance(b"\x1b[1\x9b31m", &mut sink);

    // The 0x9B should have interrupted the first CSI and started a new one.
    // Only the second CSI (from 0x9B) completes with final byte 'm'.
    assert_eq!(
        sink.csi_dispatches.len(),
        1,
        "C1 CSI mid-sequence should work when C1 is enabled"
    );
}
