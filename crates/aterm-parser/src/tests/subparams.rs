// Copyright 2026 Andrew Yates
// Author: Andrew Yates
// SPDX-License-Identifier: Apache-2.0

//! Colon subparameters: underline styles, mixed separators and the
//! subparameter mask.

use super::super::*;
use super::RecordingSink;

#[test]
fn parse_csi_colon_subparams() {
    let mut parser = Parser::new();
    let mut sink = RecordingSink::default();

    // SGR 4:3 - curly underline (colon-separated subparameter)
    parser.advance_fast(b"\x1b[4:3m", &mut sink);

    assert_eq!(sink.csi_dispatches.len(), 0);
    assert_eq!(sink.csi_dispatches_with_subparams.len(), 1);

    let (params, intermediates, final_byte, subparam_mask) = &sink.csi_dispatches_with_subparams[0];
    assert_eq!(params, &vec![4, 3]);
    assert_eq!(intermediates, &Vec::<u8>::new());
    assert_eq!(*final_byte, b'm');
    // param[1] is a subparameter.
    assert_eq!(*subparam_mask, 0b10);
}

#[test]
fn parse_csi_mixed_colon_semicolon() {
    let mut parser = Parser::new();
    let mut sink = RecordingSink::default();

    // Mixed: bold (1), then curly underline (4:3) using both ; and :
    parser.advance_fast(b"\x1b[1;4:3m", &mut sink);

    assert_eq!(sink.csi_dispatches_with_subparams.len(), 1);
    let (params, _, _, subparam_mask) = &sink.csi_dispatches_with_subparams[0];
    assert_eq!(params, &vec![1, 4, 3]);
    // Bit 2 should be set (param[2] is a subparameter of param[1])
    assert_eq!(*subparam_mask, 0b100);
}

#[test]
fn parse_csi_no_colon_no_subparams() {
    let mut parser = Parser::new();
    let mut sink = RecordingSink::default();

    // No colons - should use regular csi_dispatch
    parser.advance_fast(b"\x1b[1;4m", &mut sink);

    assert_eq!(sink.csi_dispatches.len(), 1);
    assert_eq!(sink.csi_dispatches_with_subparams.len(), 0);
    assert_eq!(sink.csi_dispatches[0].0, vec![1, 4]);
}

#[test]
fn parse_csi_subparam_flag_survives_past_index_16() {
    // MAX_PARAMS is 24; the mask must carry colon flags for indices >= 16
    // (Kakoune-style long SGR runs with trailing colon groups).
    // 16 semicolon params, then underline-color colon group 58:5:196
    // landing at indices 16..=18 (subparams at 17 and 18). Fed whole (the CSI
    // fast path) and one byte at a time (dispatch.rs finalize_param).
    let input = b"\x1b[1;2;3;4;5;6;7;8;9;10;11;12;13;14;15;16;58:5:196m";
    for byte_by_byte in [false, true] {
        let mut parser = Parser::new();
        let mut sink = RecordingSink::default();
        if byte_by_byte {
            for &b in input {
                parser.advance_fast(&[b], &mut sink);
            }
        } else {
            parser.advance_fast(input, &mut sink);
        }

        assert_eq!(sink.csi_dispatches_with_subparams.len(), 1);
        let (params, _, final_byte, subparam_mask) = &sink.csi_dispatches_with_subparams[0];
        assert_eq!(params.len(), 19, "byte_by_byte={byte_by_byte}");
        assert_eq!(&params[16..], &[58, 5, 196]);
        assert_eq!(*final_byte, b'm');
        assert_eq!(*subparam_mask, (1 << 17) | (1 << 18));
    }
}

#[test]
fn parse_csi_subparam_flag_at_last_param_index() {
    // Colon subparam landing exactly at index MAX_PARAMS - 1 (23).
    let mut parser = Parser::new();
    let mut sink = RecordingSink::default();

    parser.advance_fast(
        b"\x1b[1;2;3;4;5;6;7;8;9;10;11;12;13;14;15;16;17;18;19;20;21;22;4:3m",
        &mut sink,
    );

    assert_eq!(sink.csi_dispatches_with_subparams.len(), 1);
    let (params, _, _, subparam_mask) = &sink.csi_dispatches_with_subparams[0];
    assert_eq!(params.len(), MAX_PARAMS);
    assert_eq!(&params[22..], &[4, 3]);
    assert_eq!(*subparam_mask, 1 << 23);
}
