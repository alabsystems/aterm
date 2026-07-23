// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0
// Author: Andrew Yates

//! Integration test: verify mode recovery pattern used by hotswap (#5520).
//!
//! `restore_containment_mode()` in aterm-daemon parses a manifest mode string
//! and calls `init_mode()`. This test exercises the same contract: parse the
//! mode string from JSON, initialize it, and verify the global state.
//!
//! Regression guard: before #5520 fix, hotswap would silently reset to Master.

use aterm_containment::{
    ContainmentMode, ContainmentPolicy, McpCapability, NetworkCapability, ProcessCapability,
};

/// Simulate the hotswap recovery path: manifest contains "Containment",
/// parse it and initialize. After recovery, all gates must deny.
#[test]
fn hotswap_recovery_preserves_containment_mode() {
    // Simulate manifest JSON field: containment_mode: Some("Containment")
    let manifest_mode: Option<String> = Some("Containment".to_string());

    // This mirrors restore_containment_mode() logic:
    let mode = manifest_mode
        .as_deref()
        .and_then(|s| s.parse::<ContainmentMode>().ok())
        .expect("valid mode string should parse");

    aterm_containment::init_mode(mode).expect("first init should succeed");

    // Verify: mode is Containment, NOT Master (the pre-#5520 bug).
    assert_eq!(
        aterm_containment::current_mode(),
        ContainmentMode::Containment
    );

    // Verify: all containment gates are maximally restrictive.
    let caps = ContainmentPolicy::capabilities(aterm_containment::current_mode());
    assert_eq!(
        caps.network,
        NetworkCapability::None,
        "network must be denied"
    );
    assert_eq!(
        caps.process,
        ProcessCapability::NoFork,
        "fork must be denied"
    );
    assert_eq!(caps.mcp, McpCapability::Disabled, "MCP must be disabled");

    // Verify: second init attempt fails (OnceLock immutability).
    let result = aterm_containment::init_mode(ContainmentMode::Master);
    assert!(
        result.is_err(),
        "escalation from Containment to Master must be rejected"
    );

    // Mode must still be Containment after failed escalation.
    assert_eq!(
        aterm_containment::current_mode(),
        ContainmentMode::Containment
    );
}
