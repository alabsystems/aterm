// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0
// Author: Andrew Yates

//! Integration test: verify `init_mode_from_env` reads `ATERM_CONTAINMENT_MODE`.
//!
//! This runs in its own binary (fresh OnceLock). Setting the env var before
//! calling `init_mode_from_env` should override the default.

use aterm_containment::ContainmentMode;

#[test]
fn init_from_env_uses_env_var_over_default() {
    // Set the env var to Safety. The default is Master.
    // Single-threaded test binary — no concurrent env reads — and routed through
    // the workspace's one lock-scoped env helper (`aterm_log::env`) regardless.
    aterm_log::env::set("ATERM_CONTAINMENT_MODE", "safety");

    let mode = aterm_containment::init_mode_from_env(ContainmentMode::Master)
        .expect("init_mode_from_env should succeed");

    // Env var "safety" should override the Master default.
    assert_eq!(mode, ContainmentMode::Safety);

    // Global mode should now be Safety.
    assert_eq!(aterm_containment::current_mode(), ContainmentMode::Safety);

    // Capabilities should match Safety policy.
    let caps =
        aterm_containment::ContainmentPolicy::capabilities(aterm_containment::current_mode());
    assert_eq!(
        caps.network,
        aterm_containment::NetworkCapability::Allowlist
    );
    assert_eq!(
        caps.process,
        aterm_containment::ProcessCapability::Restricted
    );
    assert_eq!(caps.mcp, aterm_containment::McpCapability::Allowlist);
}
