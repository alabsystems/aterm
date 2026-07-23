// Copyright 2026 Andrew Yates
// Author: Andrew Yates
// SPDX-License-Identifier: Apache-2.0

//! Tests for parser UTF-8 error handling paths in advance_fast/advance_batch.
//!
//! The basic `advance()` method processes bytes through the state machine table
//! and does not perform UTF-8 reassembly. The `advance_fast()` and
//! `advance_batch()` paths do UTF-8 reassembly via `process_utf8_byte` and
//! `process_ground_special_byte`. These tests cover those error paths.

use super::super::*;
use super::RecordingSink;

// ============== Invalid Continuation Byte (advance_fast) ==============

/// A valid 2-byte UTF-8 lead (0xC3) followed by an ASCII byte instead of a
/// continuation byte should emit U+FFFD for the broken sequence, then print
/// the ASCII character normally.
#[test]
fn fast_invalid_continuation_emits_replacement_then_reprints_ascii() {
    let mut parser = Parser::new();
    let mut sink = RecordingSink::default();

    // 0xC3 expects one continuation byte (0x80..0xBF), but we give 'A' (0x41)
    parser.advance_fast(&[0xC3, 0x41], &mut sink);

    assert!(
        sink.prints.contains(&'\u{FFFD}'),
        "should emit replacement char for broken UTF-8 lead, got: {:?}",
        sink.prints
    );
    assert!(
        sink.prints.contains(&'A'),
        "ASCII byte after broken lead should be printed, got: {:?}",
        sink.prints
    );
}

/// A valid 3-byte UTF-8 lead (0xE2) with only one valid continuation byte
/// followed by an ASCII byte should emit U+FFFD and then the ASCII.
#[test]
fn fast_truncated_3byte_sequence_emits_replacement() {
    let mut parser = Parser::new();
    let mut sink = RecordingSink::default();

    // Euro sign would be 0xE2 0x82 0xAC — give only 0xE2 0x82 then 'X'
    parser.advance_fast(&[0xE2, 0x82, b'X'], &mut sink);

    assert!(
        sink.prints.contains(&'\u{FFFD}'),
        "truncated 3-byte sequence should produce replacement, got: {:?}",
        sink.prints
    );
    assert!(
        sink.prints.contains(&'X'),
        "trailing ASCII should be printed, got: {:?}",
        sink.prints
    );
    assert!(
        !sink.prints.contains(&'€'),
        "incomplete sequence should not produce euro sign"
    );
}

/// Two consecutive lead bytes: the second lead should cause the first to emit
/// U+FFFD and then the second starts a new sequence.
#[test]
fn fast_consecutive_lead_bytes_emit_replacement() {
    let mut parser = Parser::new();
    let mut sink = RecordingSink::default();

    // 0xC3 (2-byte lead), 0xC3 (another lead — interrupts), 0xA9 (valid continuation for second)
    // First 0xC3 is broken (followed by another lead), second 0xC3 + 0xA9 = 'é'
    parser.advance_fast(&[0xC3, 0xC3, 0xA9], &mut sink);

    assert!(
        sink.prints.contains(&'\u{FFFD}'),
        "first broken lead should emit replacement, got: {:?}",
        sink.prints
    );
    assert!(
        sink.prints.contains(&'é'),
        "second valid 2-byte sequence should produce 'é', got: {:?}",
        sink.prints
    );
}

// ============== Orphan Continuation Bytes (advance_fast) ==============

/// An orphan continuation byte (0xA0..0xBF) in Ground state should emit U+FFFD
/// via process_ground_special_byte.
#[test]
fn fast_orphan_continuation_byte_emits_replacement() {
    let mut parser = Parser::new();
    let mut sink = RecordingSink::default();

    // 0xA0 is a continuation byte with no preceding lead
    parser.advance_fast(&[0xA0], &mut sink);

    assert_eq!(
        sink.prints,
        vec!['\u{FFFD}'],
        "orphan continuation byte should produce single replacement"
    );
}

/// Bytes >= 0xF8 are not valid UTF-8 lead bytes and should emit U+FFFD
/// via process_ground_special_byte.
#[test]
fn fast_invalid_lead_byte_f8_emits_replacement() {
    let mut parser = Parser::new();
    let mut sink = RecordingSink::default();

    parser.advance_fast(&[0xF8], &mut sink);

    assert_eq!(
        sink.prints,
        vec!['\u{FFFD}'],
        "byte >= 0xF8 should produce replacement"
    );
}

/// 0xFF is never valid in UTF-8 and should emit U+FFFD.
#[test]
fn fast_byte_ff_emits_replacement() {
    let mut parser = Parser::new();
    let mut sink = RecordingSink::default();

    parser.advance_fast(&[0xFF], &mut sink);

    assert_eq!(
        sink.prints,
        vec!['\u{FFFD}'],
        "0xFF should produce replacement"
    );
}

// ============== Overlong Sequences (advance_fast) ==============

/// Overlong 2-byte encoding of '/' (U+002F) as 0xC0 0xAF should be rejected.
#[test]
fn fast_overlong_2byte_emits_replacement() {
    let mut parser = Parser::new();
    let mut sink = RecordingSink::default();

    // 0xC0 0xAF = overlong encoding of U+002F
    parser.advance_fast(&[0xC0, 0xAF], &mut sink);

    // Should NOT produce '/'
    assert!(
        !sink.prints.contains(&'/'),
        "overlong encoding must not produce the encoded character, got: {:?}",
        sink.prints
    );
    assert!(
        sink.prints.contains(&'\u{FFFD}'),
        "overlong sequence should produce at least one replacement, got: {:?}",
        sink.prints
    );
}

// ============== Partial UTF-8 Across Calls (advance_fast) ==============

/// A multi-byte UTF-8 sequence split across two advance_fast() calls should
/// produce the correct character, not a replacement.
#[test]
fn fast_split_utf8_across_calls_produces_correct_char() {
    let mut parser = Parser::new();
    let mut sink = RecordingSink::default();

    // Euro sign: 0xE2 0x82 0xAC — split after first byte
    parser.advance_fast(&[0xE2], &mut sink);
    parser.advance_fast(&[0x82, 0xAC], &mut sink);

    assert!(
        sink.prints.contains(&'€'),
        "split UTF-8 across calls should produce euro sign, got: {:?}",
        sink.prints
    );
    assert!(
        !sink.prints.contains(&'\u{FFFD}'),
        "valid split sequence should not produce replacement"
    );
}

/// Split a 4-byte UTF-8 sequence (emoji) across multiple advance_fast() calls.
#[test]
fn fast_split_4byte_utf8_across_calls() {
    let mut parser = Parser::new();
    let mut sink = RecordingSink::default();

    // U+1F600 (😀): 0xF0 0x9F 0x98 0x80
    parser.advance_fast(&[0xF0], &mut sink);
    parser.advance_fast(&[0x9F], &mut sink);
    parser.advance_fast(&[0x98, 0x80], &mut sink);

    assert!(
        sink.prints.contains(&'😀'),
        "split 4-byte UTF-8 should produce emoji, got: {:?}",
        sink.prints
    );
    assert!(
        !sink.prints.contains(&'\u{FFFD}'),
        "valid split 4-byte sequence should not produce replacement"
    );
}

// ============== Empty Input ==============

/// Empty input should produce zero actions and preserve state.
#[test]
fn empty_input_produces_no_actions() {
    let mut parser = Parser::new();
    let mut sink = RecordingSink::default();

    parser.advance(b"", &mut sink);

    assert!(sink.prints.is_empty());
    assert!(sink.executes.is_empty());
    assert!(sink.csi_dispatches.is_empty());
    assert!(sink.esc_dispatches.is_empty());
    assert!(sink.osc_dispatches.is_empty());
    assert_eq!(parser.state(), State::Ground);
}

/// Empty input via advance_fast should also produce zero actions.
#[test]
fn fast_empty_input_produces_no_actions() {
    let mut parser = Parser::new();
    let mut sink = RecordingSink::default();

    parser.advance_fast(b"", &mut sink);

    assert!(sink.prints.is_empty());
    assert!(sink.executes.is_empty());
    assert_eq!(parser.state(), State::Ground);
}

// ============== SUB (0x1A) Abort ==============

/// SUB (0x1A) should abort a CSI sequence, like CAN (0x18).
#[test]
fn sub_aborts_csi_sequence() {
    let mut parser = Parser::new();
    let mut sink = RecordingSink::default();

    // Start CSI, then SUB aborts, then "Hi" should print
    parser.advance(b"\x1b[31\x1aHi", &mut sink);

    assert!(sink.executes.contains(&0x1A), "SUB byte should be executed");
    assert_eq!(
        sink.csi_dispatches.len(),
        0,
        "CSI should not dispatch after SUB abort"
    );
    assert_eq!(
        sink.prints,
        vec!['H', 'i'],
        "text after SUB should print normally"
    );
}
