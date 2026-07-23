// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0
// Author: Andrew Yates

//! Integration test: Master mode ignores the allowlist entirely.
//!
//! Fresh OnceLock binary — initializes Master mode. All `is_*_allowed()`
//! functions return true regardless of allowlist state.

use aterm_containment::ContainmentMode;

#[test]
fn master_mode_allows_everything() {
    // Initialize Master mode (no allowlist needed).
    aterm_containment::init_mode(ContainmentMode::Master).expect("mode init");

    // Master mode: all operations allowed, no allowlist consulted.
    assert!(aterm_containment::is_mcp_allowed("any_tool"));
    assert!(aterm_containment::is_plugin_allowed("any_plugin"));
    assert!(aterm_containment::is_network_allowed("any.host:9999"));
    assert!(aterm_containment::is_process_allowed("/any/command"));
}
