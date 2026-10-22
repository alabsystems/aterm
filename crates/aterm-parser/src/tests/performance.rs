// Copyright 2026 Andrew Yates
// Author: Andrew Yates
// SPDX-License-Identifier: Apache-2.0

//! O(n) checks by deterministic loop-iteration counters, not wall-clock time.

use super::super::*;

/// CSI sequences of MAX_PARAMS parameters take at most one loop iteration per
/// byte.
#[test]
fn parser_handles_pathological_input() {
    let mut parser = Parser::new();
    let mut sink = NullSink;

    let mut pathological = Vec::with_capacity(64 * 1024);
    for _ in 0..1000 {
        pathological.extend_from_slice(b"\x1b[");
        for i in 0..crate::MAX_PARAMS {
            if i > 0 {
                pathological.push(b';');
            }
            pathological.extend_from_slice(b"999");
        }
        pathological.push(b'm');
    }

    let _ = take_parser_loop_iterations();

    parser.advance_fast(&pathological, &mut sink);

    let iterations = take_parser_loop_iterations();
    let input_len = pathological.len();

    // Each iteration consumes at least one byte.
    assert!(
        iterations <= input_len,
        "Parser iterations {} exceeds input length {} - possible O(n^2) behavior",
        iterations,
        input_len
    );

    assert!(
        iterations > 0,
        "Parser should have processed input (iterations=0)"
    );
}

/// An OSC over MAX_OSC_DATA (the truncation path) takes at most one iteration
/// per byte.
#[test]
fn parser_handles_large_osc() {
    let mut parser = Parser::new();
    let mut sink = NullSink;

    let payload = MAX_OSC_DATA + 4096;
    let mut osc = Vec::with_capacity(payload + 8);
    osc.extend_from_slice(b"\x1b]0;"); // OSC title
    osc.extend(std::iter::repeat_n(b'X', payload));
    osc.push(0x07); // BEL terminator

    let _ = take_parser_loop_iterations();

    parser.advance_fast(&osc, &mut sink);

    let iterations = take_parser_loop_iterations();
    let input_len = osc.len();

    assert!(
        iterations <= input_len,
        "OSC parsing iterations {} exceeds input length {}",
        iterations,
        input_len
    );

    assert!(iterations > 0, "Parser should have processed OSC input");
}
