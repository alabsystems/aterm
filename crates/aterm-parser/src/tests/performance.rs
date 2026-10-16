// Copyright 2026 Andrew Yates
// Author: Andrew Yates
// SPDX-License-Identifier: Apache-2.0

//! Performance tests (O(n) verification via deterministic counters).
//!
//! These tests use loop iteration counters instead of wall-clock timing
//! to verify O(n) complexity deterministically.

use super::super::*;

/// Verify parser handles worst-case input (all escape sequences) without
/// pathological behavior. The parser should process N bytes in O(N) time.
#[test]
fn parser_handles_pathological_input() {
    let mut parser = Parser::new();
    let mut sink = NullSink;

    // Worst case: many CSI sequences with maximum parameters
    // Each sequence: ESC [ 999;999;...;999 m  (MAX_PARAMS segments)
    let mut pathological = Vec::with_capacity(64 * 1024);
    for _ in 0..1000 {
        // Start CSI
        pathological.extend_from_slice(b"\x1b[");
        // MAX_PARAMS (24) parameters
        for i in 0..crate::MAX_PARAMS {
            if i > 0 {
                pathological.push(b';');
            }
            pathological.extend_from_slice(b"999");
        }
        // Final byte
        pathological.push(b'm');
    }

    let _ = take_parser_loop_iterations();

    parser.advance_fast(&pathological, &mut sink);

    let iterations = take_parser_loop_iterations();
    let input_len = pathological.len();

    // O(n) verification: iterations should be bounded by O(input_len).
    // Each loop iteration processes at least 1 byte, so iterations <= input_len.
    // O(n^2) would give iterations >> input_len.
    assert!(
        iterations <= input_len,
        "Parser iterations {} exceeds input length {} - possible O(n^2) behavior",
        iterations,
        input_len
    );

    // Also verify we actually processed something (sanity check)
    assert!(
        iterations > 0,
        "Parser should have processed input (iterations=0)"
    );
}

/// Verify parser handles OSC with maximum data without pathological behavior.
#[test]
fn parser_handles_large_osc() {
    let mut parser = Parser::new();
    let mut sink = NullSink;

    // Build an OSC sequence that overflows MAX_OSC_DATA so the over-cap
    // truncation path is exercised. Iteration count must still be O(n) in the
    // input length even though the buffer truncates.
    let payload = MAX_OSC_DATA + 4096;
    let mut osc = Vec::with_capacity(payload + 8);
    osc.extend_from_slice(b"\x1b]0;"); // OSC title
    osc.extend(std::iter::repeat_n(b'X', payload));
    osc.push(0x07); // BEL terminator

    let _ = take_parser_loop_iterations();

    parser.advance_fast(&osc, &mut sink);

    let iterations = take_parser_loop_iterations();
    let input_len = osc.len();

    // O(n) verification: iterations bounded by input length.
    // Each loop iteration processes at least 1 byte.
    assert!(
        iterations <= input_len,
        "OSC parsing iterations {} exceeds input length {}",
        iterations,
        input_len
    );

    // Sanity check: we processed something
    assert!(iterations > 0, "Parser should have processed OSC input");
}
