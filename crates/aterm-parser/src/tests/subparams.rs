// Copyright 2026 Andrew Yates
// Author: Andrew Yates
// SPDX-License-Identifier: Apache-2.0

//! Colon subparameter tests: underline styles (curly, dotted, dashed),
//! mixed colon/semicolon separation, and subparam mask correctness.

use super::super::*;
use super::RecordingSink;

#[test]
fn parse_csi_colon_subparams() {
    let mut parser = Parser::new();
    let mut sink = RecordingSink::default();

    // SGR 4:3 - curly underline (colon-separated subparameter)
    parser.advance(b"\x1b[4:3m", &mut sink);

    // Should call csi_dispatch_with_subparams since colons are present
    assert_eq!(sink.csi_dispatches.len(), 0);
    assert_eq!(sink.csi_dispatches_with_subparams.len(), 1);

    let (params, intermediates, final_byte, subparam_mask) = &sink.csi_dispatches_with_subparams[0];
    assert_eq!(params, &vec![4, 3]);
    assert_eq!(intermediates, &Vec::<u8>::new());
    assert_eq!(*final_byte, b'm');
    // Bit 1 should be set (param[1] is a subparameter)
    assert_eq!(*subparam_mask, 0b10);
}

#[test]
fn parse_csi_dotted_underline() {
    let mut parser = Parser::new();
    let mut sink = RecordingSink::default();

    // SGR 4:4 - dotted underline
    parser.advance(b"\x1b[4:4m", &mut sink);

    assert_eq!(sink.csi_dispatches_with_subparams.len(), 1);
    let (params, _, _, subparam_mask) = &sink.csi_dispatches_with_subparams[0];
    assert_eq!(params, &vec![4, 4]);
    assert_eq!(*subparam_mask, 0b10);
}

#[test]
fn parse_csi_dashed_underline() {
    let mut parser = Parser::new();
    let mut sink = RecordingSink::default();

    // SGR 4:5 - dashed underline
    parser.advance(b"\x1b[4:5m", &mut sink);

    assert_eq!(sink.csi_dispatches_with_subparams.len(), 1);
    let (params, _, _, subparam_mask) = &sink.csi_dispatches_with_subparams[0];
    assert_eq!(params, &vec![4, 5]);
    assert_eq!(*subparam_mask, 0b10);
}

#[test]
fn parse_csi_mixed_colon_semicolon() {
    let mut parser = Parser::new();
    let mut sink = RecordingSink::default();

    // Mixed: bold (1), then curly underline (4:3) using both ; and :
    parser.advance(b"\x1b[1;4:3m", &mut sink);

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
    parser.advance(b"\x1b[1;4m", &mut sink);

    assert_eq!(sink.csi_dispatches.len(), 1);
    assert_eq!(sink.csi_dispatches_with_subparams.len(), 0);
    assert_eq!(sink.csi_dispatches[0].0, vec![1, 4]);
}

#[test]
fn parse_csi_subparam_flag_survives_past_index_16() {
    // MAX_PARAMS is 24; the mask must carry colon flags for indices >= 16
    // (Kakoune-style long SGR runs with trailing colon groups, #subparam-u32).
    let mut parser = Parser::new();
    let mut sink = RecordingSink::default();

    // 16 semicolon params, then underline-color colon group 58:5:196
    // landing at indices 16..=18 (subparams at 17 and 18).
    parser.advance(
        b"\x1b[1;2;3;4;5;6;7;8;9;10;11;12;13;14;15;16;58:5:196m",
        &mut sink,
    );

    assert_eq!(sink.csi_dispatches_with_subparams.len(), 1);
    let (params, _, final_byte, subparam_mask) = &sink.csi_dispatches_with_subparams[0];
    assert_eq!(params.len(), 19);
    assert_eq!(&params[16..], &[58, 5, 196]);
    assert_eq!(*final_byte, b'm');
    assert_eq!(*subparam_mask, (1 << 17) | (1 << 18));
}

#[test]
fn parse_csi_subparam_flag_past_index_16_byte_by_byte() {
    // Same sequence fed one byte at a time to force the state-machine path
    // (dispatch.rs finalize_param) instead of the CSI fast path.
    let mut parser = Parser::new();
    let mut sink = RecordingSink::default();

    for &b in b"\x1b[1;2;3;4;5;6;7;8;9;10;11;12;13;14;15;16;58:5:196m" {
        parser.advance(&[b], &mut sink);
    }

    assert_eq!(sink.csi_dispatches_with_subparams.len(), 1);
    let (params, _, final_byte, subparam_mask) = &sink.csi_dispatches_with_subparams[0];
    assert_eq!(params.len(), 19);
    assert_eq!(&params[16..], &[58, 5, 196]);
    assert_eq!(*final_byte, b'm');
    assert_eq!(*subparam_mask, (1 << 17) | (1 << 18));
}

#[test]
fn parse_csi_subparam_flag_at_last_param_index() {
    // Colon subparam landing exactly at index MAX_PARAMS - 1 (23).
    let mut parser = Parser::new();
    let mut sink = RecordingSink::default();

    parser.advance(
        b"\x1b[1;2;3;4;5;6;7;8;9;10;11;12;13;14;15;16;17;18;19;20;21;22;4:3m",
        &mut sink,
    );

    assert_eq!(sink.csi_dispatches_with_subparams.len(), 1);
    let (params, _, _, subparam_mask) = &sink.csi_dispatches_with_subparams[0];
    assert_eq!(params.len(), MAX_PARAMS);
    assert_eq!(&params[22..], &[4, 3]);
    assert_eq!(*subparam_mask, 1 << 23);
}

#[test]
fn parse_csi_subparam_mask_getter() {
    let mut parser = Parser::new();
    let mut sink = RecordingSink::default();

    // After parsing, check subparam_mask via getter
    parser.advance(b"\x1b[4:3m", &mut sink);

    // Note: subparam_mask is reset on clear, which happens at start of new sequence
    // So we need to check before the next sequence starts
    // The mask should have been used in the dispatch
    assert_eq!(sink.csi_dispatches_with_subparams[0].3, 0b10);
}
