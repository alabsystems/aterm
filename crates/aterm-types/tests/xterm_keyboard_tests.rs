// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0
// Author: Andrew Yates

//! Integration tests for `XtermKeyboardState` — Default vs new() divergence,
//! disabled-state query response, and boundary clamping.
//!
//! Part of #3139: closes xterm keyboard test coverage gaps after extraction.

use aterm_types::XtermKeyboardState;

#[test]
fn test_default_vs_new_divergence() {
    // Default (derive) produces None (disabled); new() produces Some(0) (enabled level 0).
    // These are semantically different AND produce different query responses.
    let default = XtermKeyboardState::default();
    let new = XtermKeyboardState::new();

    assert_eq!(
        default.modify_other_keys(),
        None,
        "Default should be disabled (None)"
    );
    assert_eq!(
        new.modify_other_keys(),
        Some(0),
        "new() should be enabled at level 0"
    );

    // Query responses differ: disabled → no value parameter, enabled → explicit 0
    assert_eq!(default.query_modify_other_keys_response(), "\x1b[>4m");
    assert_eq!(new.query_modify_other_keys_response(), "\x1b[>4;0m");

    // Neither counts as "enabled" for modifier encoding purposes
    assert!(!default.modify_other_keys_enabled());
    assert!(!new.modify_other_keys_enabled());
}

#[test]
fn test_query_response_when_disabled() {
    let mut state = XtermKeyboardState::new();
    state.disable_modify_other_keys();
    assert_eq!(state.modify_other_keys(), None);
    // Disabled reports no value parameter (per xterm spec: CSI > 4 m)
    assert_eq!(state.query_modify_other_keys_response(), "\x1b[>4m");
}

#[test]
fn test_boundary_clamp_values() {
    let mut state = XtermKeyboardState::new();

    // Exact boundary: 2 is max for modify_other_keys
    state.set_modify_other_keys(2);
    assert_eq!(state.modify_other_keys(), Some(2));

    // One above boundary
    state.set_modify_other_keys(3);
    assert_eq!(state.modify_other_keys(), Some(2));

    // u8::MAX
    state.set_modify_other_keys(u8::MAX);
    assert_eq!(state.modify_other_keys(), Some(2));

    // Exact boundary: 1 is max for format_other_keys
    state.set_format_other_keys(1);
    assert_eq!(state.format_other_keys(), 1);

    state.set_format_other_keys(2);
    assert_eq!(state.format_other_keys(), 1);

    state.set_format_other_keys(u8::MAX);
    assert_eq!(state.format_other_keys(), 1);
}
