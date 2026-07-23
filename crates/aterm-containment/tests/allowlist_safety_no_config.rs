// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0
// Author: Andrew Yates

//! Integration test: Safety mode without allowlist denies everything (fail-closed).
//!
//! Fresh OnceLock binary — initializes Safety mode but does NOT call
//! `init_allowlist()`. All `is_*_allowed()` functions must return false.

use aterm_containment::ContainmentMode;

#[test]
fn safety_mode_without_allowlist_denies_all() {
    // Initialize Safety mode without providing an allowlist.
    aterm_containment::init_mode(ContainmentMode::Safety).expect("mode init");

    // No init_allowlist() call — fail-closed behavior.

    // All MCP tools denied.
    assert!(!aterm_containment::is_mcp_allowed("read_file"));
    assert!(!aterm_containment::is_mcp_allowed("write_file"));

    // All plugins denied.
    assert!(!aterm_containment::is_plugin_allowed("spell-check"));

    // All network targets denied.
    assert!(!aterm_containment::is_network_allowed("localhost:8080"));
    assert!(!aterm_containment::is_network_allowed(
        "unix:/tmp/aterm.sock"
    ));

    // All commands denied.
    assert!(!aterm_containment::is_process_allowed("/bin/bash"));
    assert!(!aterm_containment::is_process_allowed("/bin/zsh"));
}
