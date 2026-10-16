// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0
// Author: Andrew Yates

//! Terminal-coupled extras performance test.

/// `take_response()` keeps the buffer's allocation for the next `process()` cycle.
#[test]
fn take_response_preserves_capacity() {
    use crate::terminal::Terminal;

    let mut term = Terminal::new(24, 80);

    term.process(b"\x1b[5n"); // DSR — terminal should respond with \x1b[0n
    let initial_capacity = term.response_buffer_capacity();
    assert!(
        initial_capacity > 0,
        "DSR response should allocate response buffer capacity before take_response"
    );

    let response = term.take_response();
    assert!(response.is_some(), "should have response after DSR");

    assert_eq!(
        term.response_buffer_capacity(),
        initial_capacity,
        "take_response should preserve response buffer capacity"
    );

    term.process(b"\x1b[5n");
    let response2 = term.take_response();
    assert!(response2.is_some(), "should have response after second DSR");
    assert_eq!(
        response, response2,
        "both DSR responses should be identical"
    );
}
