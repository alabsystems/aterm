// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0
// Author: Andrew Yates

//! Terminal-coupled extras performance test.
//!
//! Grid-only extras/reflow complexity tests migrated to aterm-grid in
//! Batch 2 (#6556). This file retains the single test that uses
//! `crate::terminal::Terminal`.

/// Verify that `take_response()` preserves buffer capacity (#4073, #4544).
///
/// `clone()` + `clear()` returns the data to the caller while the internal
/// buffer retains its heap allocation. The next `process()` cycle reuses the
/// existing capacity instead of re-allocating from scratch.
#[test]
fn take_response_preserves_capacity() {
    use crate::terminal::Terminal;

    let mut term = Terminal::new(24, 80);

    // Generate a response by sending a Device Status Report request.
    term.process(b"\x1b[5n"); // DSR — terminal should respond with \x1b[0n
    let initial_capacity = term.response_buffer_capacity();
    assert!(
        initial_capacity > 0,
        "DSR response should allocate response buffer capacity before take_response"
    );

    let response = term.take_response();
    assert!(response.is_some(), "should have response after DSR");

    // After take_response, the internal buffer retains its heap allocation.
    assert_eq!(
        term.response_buffer_capacity(),
        initial_capacity,
        "take_response should preserve response buffer capacity"
    );

    // Second cycle reuses the existing allocation.
    term.process(b"\x1b[5n");
    let response2 = term.take_response();
    assert!(response2.is_some(), "should have response after second DSR");
    assert_eq!(
        response, response2,
        "both DSR responses should be identical"
    );
}
