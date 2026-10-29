// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0

//! CSI sequence tests: parameter parsing, private markers, overflow clamping,
//! and the CSI fast path's parity with the per-byte state machine.

use super::super::*;
use super::RecordingSink;

#[test]
fn parse_csi_many_params() {
    use crate::MAX_PARAMS;
    let mut parser = Parser::new();
    let mut sink = RecordingSink::default();

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
    assert_eq!(params[0], 1, "first param preserved");
    assert_eq!(params[23], 24, "last retained param is 24 (25,26 dropped)");
}

#[test]
fn parse_csi_kakoune_sgr() {
    let mut parser = Parser::new();
    let mut sink = RecordingSink::default();

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

#[test]
fn csi_single_sequences_dispatch_once() {
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

    parser.advance_fast(b"Hello\x1b[31mWorld\x1b[0m!", &mut sink);

    assert_eq!(sink.prints.len(), 11); // "Hello" + "World" + "!"
    assert_eq!(sink.csi_dispatches.len(), 2);
    assert_eq!(sink.csi_dispatches[0].0, vec![31]);
    assert_eq!(sink.csi_dispatches[1].0, vec![0]);
}

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
    parser_per_byte.assert_invariants();
    parser_fast.assert_invariants();
    assert_eq!(
        (
            parser_per_byte.state,
            parser_per_byte.params.as_slice(),
            parser_per_byte.intermediates.as_slice(),
            parser_per_byte.subparam_mask,
            parser_per_byte.current_param,
            parser_per_byte.param_started,
            parser_per_byte.last_was_colon,
        ),
        (
            parser_fast.state,
            parser_fast.params.as_slice(),
            parser_fast.intermediates.as_slice(),
            parser_fast.subparam_mask,
            parser_fast.current_param,
            parser_fast.param_started,
            parser_fast.last_was_colon,
        ),
        "parser CSI scratch state mismatch between per-byte and whole-slice reads"
    );
}

#[test]
fn csi_fast_path_clears_aborted_param_scratch_matching_per_byte() {
    for input in [
        b"\x1b[31\x18\x1b[m".as_slice(),
        b"\x1b[31\x18\x1b[5m",
        b"\x1b[31\x18\x1b[42m",
        b"\x1b[4:3\x18\x1b[5m",
        b"\x1b[4:3m\x1b[5m",
        b"\x1b[4:3m\x1b[1;2m",
        b"\x1b[1;4:3m",
    ] {
        assert_whole_read_matches_per_byte(input);
    }
}

#[test]
fn csi_fast_path_falls_back_when_sequence_exceeds_64_bytes() {
    let mut input = Vec::from(&b"\x1b["[..]);
    for _ in 0..33 {
        input.extend_from_slice(b"1;");
    }
    input.extend_from_slice(b"1mZ");

    assert_whole_read_matches_per_byte(&input);
}

#[test]
fn csi_fast_path_falls_back_on_unknown_byte_in_param_scan() {
    let input = b"\x1b[1\x7fmZ";
    assert_whole_read_matches_per_byte(input);
}

#[test]
fn csi_fast_path_falls_back_on_unexpected_byte_after_intermediate() {
    let input = b"\x1b[1$2mZ";
    assert_whole_read_matches_per_byte(input);
}

#[test]
fn csi_fast_path_trailing_empty_colon_subparam_matches_per_byte() {
    let trailing_cases = [
        b"\x1b[1;2:m".as_slice(),
        b"\x1b[1;4:m",
        b"\x1b[1;4:3:m",
        b"\x1b[1;2: q",
    ];
    for input in trailing_cases {
        assert_whole_read_matches_per_byte(input);
    }

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
