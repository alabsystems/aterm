// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0
// Author: Andrew Yates

//! Integration test: `init_mode_from_flag` takes the flag's mode over the default.
//!
//! This runs in its own binary (fresh OnceLock).

use aterm_containment::ContainmentMode;

#[test]
fn init_from_flag_uses_the_flag_over_default() {
    let mode = aterm_containment::init_mode_from_flag(Some("safety"), ContainmentMode::Master)
        .expect("init_mode_from_flag should succeed");

    // The flag's "safety" overrides the Master default.
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
