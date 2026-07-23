// Copyright 2026 Andrew Yates
// Author: Andrew Yates
// SPDX-License-Identifier: Apache-2.0

//! CSI sequence tests: parameter parsing, private markers, overflow clamping,
//! and fast-path parity with the basic parser.

use super::super::*;
use super::RecordingSink;

// ============== CSI Tests ==============

#[test]
fn parse_csi_sequence() {
    let mut parser = Parser::new();
    let mut sink = RecordingSink::default();

    parser.advance(b"\x1b[31m", &mut sink);

    assert_eq!(sink.csi_dispatches.len(), 1);
    assert_eq!(sink.csi_dispatches[0], (vec![31], vec![], b'm'));
}

#[test]
fn parse_csi_multiple_params() {
    let mut parser = Parser::new();
    let mut sink = RecordingSink::default();

    // SGR with multiple params: bold, red foreground
    parser.advance(b"\x1b[1;31m", &mut sink);

    assert_eq!(sink.csi_dispatches.len(), 1);
    assert_eq!(sink.csi_dispatches[0], (vec![1, 31], vec![], b'm'));
}

#[test]
fn parse_csi_no_params() {
    let mut parser = Parser::new();
    let mut sink = RecordingSink::default();

    // Cursor Home with no params
    parser.advance(b"\x1b[H", &mut sink);

    assert_eq!(sink.csi_dispatches.len(), 1);
    assert_eq!(sink.csi_dispatches[0], (vec![], vec![], b'H'));
}

#[test]
fn parse_csi_private_marker() {
    let mut parser = Parser::new();
    let mut sink = RecordingSink::default();

    // DEC Private Mode Set (e.g., ?1049h for alternate screen)
    parser.advance(b"\x1b[?1049h", &mut sink);

    assert_eq!(sink.csi_dispatches.len(), 1);
    // The '?' should be collected as intermediate
    assert_eq!(sink.csi_dispatches[0].2, b'h');
}

#[test]
fn parse_csi_large_param() {
    let mut parser = Parser::new();
    let mut sink = RecordingSink::default();

    // Parameter larger than u16::MAX should be clamped
    parser.advance(b"\x1b[99999m", &mut sink);

    assert_eq!(sink.csi_dispatches.len(), 1);
    assert_eq!(sink.csi_dispatches[0].0[0], 65535); // u16::MAX
}

#[test]
fn parse_csi_many_params() {
    use crate::MAX_PARAMS;
    let mut parser = Parser::new();
    let mut sink = RecordingSink::default();

    // More than MAX_PARAMS parameters (only the first MAX_PARAMS are kept).
    // 26 params 1..=26; with MAX_PARAMS = 24 the last two (25, 26) are dropped.
    parser.advance(
        b"\x1b[1;2;3;4;5;6;7;8;9;10;11;12;13;14;15;16;17;18;19;20;21;22;23;24;25;26m",
        &mut sink,
    );

    assert_eq!(
        sink.csi_dispatches.len(),
        1,
        "should dispatch exactly one CSI sequence"
    );
    let params = &sink.csi_dispatches[0].0;
    assert_eq!(params.len(), MAX_PARAMS, "params truncated to MAX_PARAMS");
    assert_eq!(
        MAX_PARAMS, 24,
        "MAX_PARAMS pinned at 24 (xterm/ghostty parity)"
    );
    // Verify first params survived truncation in order
    assert_eq!(params[0], 1, "first param preserved");
    assert_eq!(params[23], 24, "last retained param is 24 (25,26 dropped)");
}

#[test]
fn parse_csi_kakoune_sgr() {
    // Regression for the 16->24 MAX_PARAMS raise: Kakoune emits a 17-parameter
    // SGR (truecolor fg + truecolor bg + attrs). At the old cap of 16 the 17th
    // parameter was truncated and the styling dropped. It must now be retained.
    let mut parser = Parser::new();
    let mut sink = RecordingSink::default();

    // 17 params: SGR 4:3 (curly underline) ; 38:2::R:G:B ; 48:2::R:G:B ...
    parser.advance(
        b"\x1b[0;4;38;2;255;128;0;48;2;16;32;48;1;3;9;53;58m",
        &mut sink,
    );

    assert_eq!(sink.csi_dispatches.len(), 1, "one CSI dispatch");
    let params = &sink.csi_dispatches[0].0;
    assert_eq!(
        params.len(),
        17,
        "all 17 Kakoune SGR params retained (not truncated)"
    );
    assert_eq!(params[0], 0, "first param (reset) preserved");
    assert_eq!(
        params[16], 58,
        "17th param (SGR 58 underline color) preserved"
    );
}

// ============== CSI Fast Path Tests ==============

#[test]
fn csi_fast_path_simple_sgr() {
    let mut parser = Parser::new();
    let mut sink = RecordingSink::default();

    // Use advance_fast to test the CSI fast path
    parser.advance_fast(b"\x1b[31m", &mut sink);

    assert_eq!(sink.csi_dispatches.len(), 1);
    assert_eq!(sink.csi_dispatches[0], (vec![31], vec![], b'm'));
}

#[test]
fn csi_fast_path_256_color() {
    let mut parser = Parser::new();
    let mut sink = RecordingSink::default();

    // 256-color foreground: ESC[38;5;196m
    parser.advance_fast(b"\x1b[38;5;196m", &mut sink);

    assert_eq!(sink.csi_dispatches.len(), 1);
    assert_eq!(sink.csi_dispatches[0], (vec![38, 5, 196], vec![], b'm'));
}

#[test]
fn csi_fast_path_true_color() {
    let mut parser = Parser::new();
    let mut sink = RecordingSink::default();

    // RGB foreground: ESC[38;2;255;128;64m
    parser.advance_fast(b"\x1b[38;2;255;128;64m", &mut sink);

    assert_eq!(sink.csi_dispatches.len(), 1);
    assert_eq!(
        sink.csi_dispatches[0],
        (vec![38, 2, 255, 128, 64], vec![], b'm')
    );
}

#[test]
fn csi_fast_path_private_marker() {
    let mut parser = Parser::new();
    let mut sink = RecordingSink::default();

    // Private mode set: ESC[?1049h
    parser.advance_fast(b"\x1b[?1049h", &mut sink);

    assert_eq!(sink.csi_dispatches.len(), 1);
    assert_eq!(sink.csi_dispatches[0].0, vec![1049]);
    assert_eq!(sink.csi_dispatches[0].1, vec![b'?']);
    assert_eq!(sink.csi_dispatches[0].2, b'h');
}

#[test]
fn csi_fast_path_cursor_position() {
    let mut parser = Parser::new();
    let mut sink = RecordingSink::default();

    // Cursor position: ESC[10;20H
    parser.advance_fast(b"\x1b[10;20H", &mut sink);

    assert_eq!(sink.csi_dispatches.len(), 1);
    assert_eq!(sink.csi_dispatches[0], (vec![10, 20], vec![], b'H'));
}

#[test]
fn csi_fast_path_no_params() {
    let mut parser = Parser::new();
    let mut sink = RecordingSink::default();

    // Cursor home: ESC[H
    parser.advance_fast(b"\x1b[H", &mut sink);

    assert_eq!(sink.csi_dispatches.len(), 1);
    assert_eq!(sink.csi_dispatches[0], (vec![], vec![], b'H'));
}

#[test]
fn csi_fast_path_multiple_sequences() {
    let mut parser = Parser::new();
    let mut sink = RecordingSink::default();

    // Multiple sequences in a row (escape-heavy workload)
    parser.advance_fast(b"\x1b[38;5;196m\x1b[48;5;21m\x1b[1;4;5m\x1b[0m", &mut sink);

    assert_eq!(sink.csi_dispatches.len(), 4);
    assert_eq!(sink.csi_dispatches[0].0, vec![38, 5, 196]);
    assert_eq!(sink.csi_dispatches[1].0, vec![48, 5, 21]);
    assert_eq!(sink.csi_dispatches[2].0, vec![1, 4, 5]);
    assert_eq!(sink.csi_dispatches[3].0, vec![0]);
}

#[test]
fn csi_fast_path_interleaved_with_text() {
    let mut parser = Parser::new();
    let mut sink = RecordingSink::default();

    // Text interleaved with CSI sequences
    parser.advance_fast(b"Hello\x1b[31mWorld\x1b[0m!", &mut sink);

    assert_eq!(sink.prints.len(), 11); // "Hello" + "World" + "!"
    assert_eq!(sink.csi_dispatches.len(), 2);
    assert_eq!(sink.csi_dispatches[0].0, vec![31]);
    assert_eq!(sink.csi_dispatches[1].0, vec![0]);
}

#[test]
fn csi_fast_path_large_param() {
    let mut parser = Parser::new();
    let mut sink = RecordingSink::default();

    // Large parameter should be clamped to u16::MAX
    parser.advance_fast(b"\x1b[99999m", &mut sink);

    assert_eq!(sink.csi_dispatches.len(), 1);
    assert_eq!(sink.csi_dispatches[0].0[0], 65535);
}

#[test]
fn csi_fast_path_matches_basic_parser() {
    // Verify fast path produces same results as basic parser
    let test_cases = [
        b"\x1b[31m".as_slice(),
        b"\x1b[1;31m",
        b"\x1b[38;5;196m",
        b"\x1b[38;2;255;128;64m",
        b"\x1b[?1049h",
        b"\x1b[10;20H",
        b"\x1b[H",
        b"\x1b[0m",
    ];

    for input in test_cases {
        let mut parser_basic = Parser::new();
        let mut sink_basic = RecordingSink::default();
        parser_basic.advance(input, &mut sink_basic);

        let mut parser_fast = Parser::new();
        let mut sink_fast = RecordingSink::default();
        parser_fast.advance_fast(input, &mut sink_fast);

        assert_eq!(
            sink_basic.csi_dispatches, sink_fast.csi_dispatches,
            "Mismatch for input: {:?}",
            input
        );
    }
}

fn assert_fast_parser_matches_basic(input: &[u8]) {
    let mut parser_basic = Parser::new();
    let mut sink_basic = RecordingSink::default();
    parser_basic.advance(input, &mut sink_basic);

    let mut parser_fast = Parser::new();
    let mut sink_fast = RecordingSink::default();
    parser_fast.advance_fast(input, &mut sink_fast);

    assert_eq!(sink_basic.prints, sink_fast.prints, "prints mismatch");
    assert_eq!(sink_basic.executes, sink_fast.executes, "executes mismatch");
    assert_eq!(
        sink_basic.csi_dispatches, sink_fast.csi_dispatches,
        "CSI dispatch mismatch"
    );
    assert_eq!(
        sink_basic.csi_dispatches_with_subparams, sink_fast.csi_dispatches_with_subparams,
        "CSI subparam dispatch mismatch"
    );
    assert_eq!(
        sink_basic.esc_dispatches, sink_fast.esc_dispatches,
        "ESC dispatch mismatch"
    );
    assert_eq!(
        sink_basic.osc_dispatches, sink_fast.osc_dispatches,
        "OSC dispatch mismatch"
    );
    assert_eq!(
        sink_basic.dcs_hooks, sink_fast.dcs_hooks,
        "DCS hook mismatch"
    );
    assert_eq!(sink_basic.dcs_puts, sink_fast.dcs_puts, "DCS put mismatch");
    assert_eq!(
        sink_basic.dcs_unhooks, sink_fast.dcs_unhooks,
        "DCS unhook mismatch"
    );
    assert_eq!(
        sink_basic.apc_starts, sink_fast.apc_starts,
        "APC start mismatch"
    );
    assert_eq!(sink_basic.apc_data, sink_fast.apc_data, "APC data mismatch");
    assert_eq!(sink_basic.apc_ends, sink_fast.apc_ends, "APC end mismatch");
}

#[test]
fn csi_fast_path_falls_back_when_sequence_exceeds_64_bytes() {
    // Trigger `try_parse_csi_fast` length guard (`final_pos > 64`) and verify
    // fast-path fallback preserves behavior.
    let mut input = Vec::from(&b"\x1b["[..]);
    for _ in 0..33 {
        input.extend_from_slice(b"1;");
    }
    input.extend_from_slice(b"1mZ");

    assert_fast_parser_matches_basic(&input);
}

#[test]
fn csi_fast_path_falls_back_on_unknown_byte_in_param_scan() {
    // DEL (0x7F) is rejected by the fast parser param scanner.
    // Ensure fallback path handles the sequence exactly like `advance`.
    let input = b"\x1b[1\x7fmZ";
    assert_fast_parser_matches_basic(input);
}

#[test]
fn csi_fast_path_falls_back_on_unexpected_byte_after_intermediate() {
    // '$' enters intermediate collection, then '2' is unexpected in that phase.
    let input = b"\x1b[1$2mZ";
    assert_fast_parser_matches_basic(input);
}

#[test]
fn csi_fast_path_trailing_empty_colon_subparam_matches_basic() {
    // A trailing ':' immediately before the final byte (or an intermediate)
    // must NOT materialize a phantom empty subparameter on the `D;` fast path.
    // The reference state machine only finalizes a pending colon value when a
    // subsequent ';'/' :' separator follows, so a trailing colon is dropped and
    // CsiDispatch fires plain. Regression for the fast/slow divergence where
    // `ESC[1;2:m` wrongly produced csi_dispatch_with_subparams([1,2,0],'m').
    let trailing_cases = [
        b"\x1b[1;2:m".as_slice(),
        b"\x1b[1;4:m",
        b"\x1b[1;4:3:m",
        b"\x1b[1;2: q",
    ];
    for input in trailing_cases {
        assert_fast_parser_matches_basic(input);
    }

    // Control cases: a separator follows the empty colon value, so the empty
    // subparameter IS materialized on both paths and they must still agree.
    let mid_cases = [
        b"\x1b[1;2:;3m".as_slice(),
        b"\x1b[1;4:3m",
        b"\x1b[4:3m",
        b"\x1b[1;2::3m",
    ];
    for input in mid_cases {
        assert_fast_parser_matches_basic(input);
    }
}
