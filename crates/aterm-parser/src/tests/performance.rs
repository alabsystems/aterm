// Copyright 2026 Andrew Yates
// Author: Andrew Yates
// SPDX-License-Identifier: Apache-2.0

//! Performance tests (O(n) verification via deterministic counters).
//!
//! These tests use loop iteration counters instead of wall-clock timing
//! to verify O(n) complexity deterministically. See #1572.

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

    // Reset counter before measurement
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

    // Reset counter before measurement
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

/// Verify linear scaling by comparing iteration counts for different sizes.
#[test]
fn parser_linear_scaling() {
    fn measure_iterations(size: usize) -> (usize, usize) {
        // Generate worst-case input: alternating escape and character
        let mut data = Vec::with_capacity(size);
        while data.len() < size {
            data.extend_from_slice(b"\x1b[mX"); // 4 bytes: ESC [ m X
        }
        data.truncate(size);

        let mut parser = Parser::new();
        let mut sink = NullSink;

        // Reset counter before measurement
        let _ = take_parser_loop_iterations();

        parser.advance_fast(&data, &mut sink);

        let iterations = take_parser_loop_iterations();
        (iterations, data.len())
    }

    let (small_iters, small_len) = measure_iterations(1000);
    let (large_iters, large_len) = measure_iterations(10000);

    // O(n) verification: iterations-per-byte should be roughly constant.
    // For O(n), the ratio of (large_iters / large_len) to (small_iters / small_len)
    // should be close to 1.0.
    // For O(n^2), larger inputs would have many more iterations per byte.
    let small_ratio = small_iters as f64 / small_len as f64;
    let large_ratio = large_iters as f64 / large_len as f64;
    let scaling_factor = large_ratio / small_ratio.max(0.001);

    // Linear scaling: factor should be close to 1.0
    // Allow up to 2.0 for constant factors and SIMD effects
    // O(n^2) would give scaling_factor ~= 10.0 (since large_len / small_len = 10)
    assert!(
        scaling_factor < 2.0,
        "Scaling factor {:.2}x suggests non-linear behavior \
         (small: {}/{} = {:.3}, large: {}/{} = {:.3})",
        scaling_factor,
        small_iters,
        small_len,
        small_ratio,
        large_iters,
        large_len,
        large_ratio
    );

    // Also verify iterations are reasonable (not zero, not excessive)
    assert!(small_iters > 0, "Small input should have iterations");
    assert!(large_iters > 0, "Large input should have iterations");
}
