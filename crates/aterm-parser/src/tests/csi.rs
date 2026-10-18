// Copyright 2026 Andrew Yates
// Author: Andrew Yates
// SPDX-License-Identifier: Apache-2.0

//! CSI sequence tests: parameter parsing, private markers, overflow clamping,
//! and the CSI fast path's parity with the per-byte state machine.

use super::super::*;
use super::RecordingSink;

// ============== CSI Tests ==============

#[test]
fn parse_csi_many_params() {
    use crate::MAX_PARAMS;
    let mut parser = Parser::new();
    let mut sink = RecordingSink::default();

    // More than MAX_PARAMS parameters (only the first MAX_PARAMS are kept).
    // 26 params 1..=26; with MAX_PARAMS = 24 the last two (25, 26) are dropped.
    parser.advance_fast(
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
    // Verify first params survived truncation in order
    assert_eq!(params[0], 1, "first param preserved");
    assert_eq!(params[23], 24, "last retained param is 24 (25,26 dropped)");
}

#[test]
fn parse_csi_kakoune_sgr() {
    // Kakoune emits a 17-parameter SGR (truecolor fg + bg + attrs); a cap of
    // 16 truncated the 17th and dropped the styling.
    let mut parser = Parser::new();
    let mut sink = RecordingSink::default();

    // 17 params: SGR 4:3 (curly underline) ; 38:2::R:G:B ; 48:2::R:G:B ...
    parser.advance_fast(
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
fn csi_single_sequences_dispatch_once() {
    // One sequence -> exactly one CSI dispatch of (params, intermediates,
    // final).
    let rows: &[(&str, &[u8], &[u16], &[u8], u8)] = &[
        ("simple SGR", b"\x1b[31m", &[31], &[], b'm'),
        ("two params", b"\x1b[1;31m", &[1, 31], &[], b'm'),
        ("SGR reset", b"\x1b[0m", &[0], &[], b'm'),
        ("256-color fg", b"\x1b[38;5;196m", &[38, 5, 196], &[], b'm'),
        (
            "RGB fg",
            b"\x1b[38;2;255;128;64m",
            &[38, 2, 255, 128, 64],
            &[],
            b'm',
        ),
        ("private mode set", b"\x1b[?1049h", &[1049], b"?", b'h'),
        ("cursor position", b"\x1b[10;20H", &[10, 20], &[], b'H'),
        ("cursor home, no params", b"\x1b[H", &[], &[], b'H'),
    ];
    for &(label, input, params, intermediates, final_byte) in rows {
        let mut parser = Parser::new();
        let mut sink = RecordingSink::default();
        parser.advance_fast(input, &mut sink);
        assert_eq!(
            sink.csi_dispatches,
            vec![(params.to_vec(), intermediates.to_vec(), final_byte)],
            "{label}"
        );
    }
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

/// `input` fed whole (the CSI fast path) and one byte per read (the per-byte
/// state machine, since the fast path needs the whole sequence in one read)
/// yield the same actions.
fn assert_whole_read_matches_per_byte(input: &[u8]) {
    let mut parser_per_byte = Parser::new();
    let mut sink_per_byte = RecordingSink::default();
    for byte in input {
        parser_per_byte.advance_fast(std::slice::from_ref(byte), &mut sink_per_byte);
    }

    let mut parser_fast = Parser::new();
    let mut sink_fast = RecordingSink::default();
    parser_fast.advance_fast(input, &mut sink_fast);

    assert_eq!(sink_per_byte.prints, sink_fast.prints, "prints mismatch");
    assert_eq!(
        sink_per_byte.executes, sink_fast.executes,
        "executes mismatch"
    );
    assert_eq!(
        sink_per_byte.csi_dispatches, sink_fast.csi_dispatches,
        "CSI dispatch mismatch"
    );
    assert_eq!(
        sink_per_byte.csi_dispatches_with_subparams, sink_fast.csi_dispatches_with_subparams,
        "CSI subparam dispatch mismatch"
    );
    assert_eq!(
        sink_per_byte.esc_dispatches, sink_fast.esc_dispatches,
        "ESC dispatch mismatch"
    );
    assert_eq!(
        sink_per_byte.osc_dispatches, sink_fast.osc_dispatches,
        "OSC dispatch mismatch"
    );
    assert_eq!(
        sink_per_byte.dcs_hooks, sink_fast.dcs_hooks,
        "DCS hook mismatch"
    );
    assert_eq!(
        sink_per_byte.dcs_puts, sink_fast.dcs_puts,
        "DCS put mismatch"
    );
    assert_eq!(
        sink_per_byte.dcs_unhooks, sink_fast.dcs_unhooks,
        "DCS unhook mismatch"
    );
    assert_eq!(
        sink_per_byte.apc_starts, sink_fast.apc_starts,
        "APC start mismatch"
    );
    assert_eq!(
        sink_per_byte.apc_data, sink_fast.apc_data,
        "APC data mismatch"
    );
    assert_eq!(
        sink_per_byte.apc_ends, sink_fast.apc_ends,
        "APC end mismatch"
    );
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

    assert_whole_read_matches_per_byte(&input);
}

#[test]
fn csi_fast_path_falls_back_on_unknown_byte_in_param_scan() {
    // DEL (0x7F) is rejected by the fast parser param scanner.
    // Ensure fallback path handles the sequence exactly like the per-byte path.
    let input = b"\x1b[1\x7fmZ";
    assert_whole_read_matches_per_byte(input);
}

#[test]
fn csi_fast_path_falls_back_on_unexpected_byte_after_intermediate() {
    // '$' enters intermediate collection, then '2' is unexpected in that phase.
    let input = b"\x1b[1$2mZ";
    assert_whole_read_matches_per_byte(input);
}

#[test]
fn csi_fast_path_trailing_empty_colon_subparam_matches_per_byte() {
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
        assert_whole_read_matches_per_byte(input);
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
        assert_whole_read_matches_per_byte(input);
    }
}

/// A parameter accumulates with saturating arithmetic and is clamped to
/// `u16::MAX` when it closes: `ESC [ <digits> m` dispatches exactly
/// `min(value, 65535)` for every digit repeated up to 24 times
/// (far past `u32::MAX`), and for the values past `u32::MAX` that a wrapping
/// accumulator would bring back small (`2^32` would read as 0, `2^32 + 5` as
/// 5, `10 * 2^32` as 0).
#[test]
fn parameter_digits_saturate_and_finalize_clamps() {
    let mut runs: Vec<String> = (0..=9u8)
        .flat_map(|digit| (1..=24).map(move |run| char::from(b'0' + digit).to_string().repeat(run)))
        .collect();
    runs.extend(
        [
            "4294967296",
            "4294967297",
            "4294967301",
            "8589934593",
            "42949672960",
        ]
        .map(String::from),
    );
    for digits in &runs {
        let value = digits.parse::<u128>().unwrap();
        let expected = u16::try_from(value.min(u128::from(u16::MAX))).unwrap();
        let sequence = format!("\x1b[{digits}m");
        let mut parser = Parser::new();
        let mut sink = RecordingSink::default();
        parser.advance_fast(sequence.as_bytes(), &mut sink);
        assert_eq!(
            sink.csi_dispatches,
            vec![(vec![expected], vec![], b'm')],
            "{digits}"
        );
    }
}

/// `add_param_digit` and `finalize_param` over the edges of the accumulator:
/// a digit multiplies and adds with saturation (exactly at and around
/// `u32::MAX / 10`, where it starts to saturate), a `;` or `:` closes the
/// parameter as `min(value, 65535)` and resets the accumulator, and
/// `finalize_param` clamps every value from 0 to 70,000 and `u32::MAX` — and
/// still resets when the list is full and the value is dropped.
#[test]
fn add_param_digit_saturates_and_finalize_param_clamps() {
    let clamp = |value: u32| u16::try_from(value.min(u32::from(u16::MAX))).unwrap();
    let edge = u32::MAX / 10;
    let initials = [0, 1, u32::MAX].into_iter().chain(edge - 10..=edge + 10);
    for initial in initials {
        for byte in (b'0'..=b'9').chain([b';', b':']) {
            let mut parser = Parser::new();
            parser.current_param = initial;
            parser.param_started = true;
            parser.add_param_digit(byte);
            if byte.is_ascii_digit() {
                assert_eq!(
                    parser.current_param,
                    initial
                        .saturating_mul(10)
                        .saturating_add(u32::from(byte - b'0')),
                    "{initial} then {}",
                    byte as char
                );
                assert!(parser.param_started);
                assert!(parser.params.is_empty());
            } else {
                assert_eq!(parser.params.as_slice(), &[clamp(initial)][..]);
                assert_eq!((parser.current_param, parser.param_started), (0, false));
                assert_eq!(parser.last_was_colon, byte == b':');
            }
        }
    }
    for value in (0..=70_000).chain([u32::MAX]) {
        let mut parser = Parser::new();
        parser.current_param = value;
        parser.param_started = true;
        parser.finalize_param();
        assert_eq!(parser.params.as_slice(), &[clamp(value)][..], "{value}");
        assert_eq!((parser.current_param, parser.param_started), (0, false));
    }
    let mut full = Parser::new();
    for _ in 0..crate::MAX_PARAMS {
        full.params.push(1);
    }
    full.current_param = u32::MAX;
    full.param_started = true;
    full.finalize_param();
    assert_eq!(full.params.len(), crate::MAX_PARAMS);
    assert_eq!((full.current_param, full.param_started), (0, false));
}
