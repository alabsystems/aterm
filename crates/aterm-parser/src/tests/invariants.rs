// Copyright 2026 Andrew Yates
// Author: Andrew Yates
// SPDX-License-Identifier: Apache-2.0

//! Runtime invariant tests (Phase 5): verify assert_invariants holds after
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
