// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0
// Author: Andrew Yates

//! Integration test: verify `init_mode_from_env` uses default when env var unset.
//!
//! Fresh OnceLock (separate binary). With no `ATERM_CONTAINMENT_MODE` env var,
//! the function should fall back to the provided default.

use aterm_containment::ContainmentMode;

#[test]
fn init_from_env_uses_default_when_var_unset() {
    // Ensure env var is NOT set.
    unsafe {
        std::env::remove_var("ATERM_CONTAINMENT_MODE");
    }

    let mode = aterm_containment::init_mode_from_env(ContainmentMode::User)
        .expect("init_mode_from_env should succeed with default");

    // Should use the provided default (User).
    assert_eq!(mode, ContainmentMode::User);

    // Global mode should now be User.
    assert_eq!(aterm_containment::current_mode(), ContainmentMode::User);

    // Verify capabilities match User policy.
    let caps =
        aterm_containment::ContainmentPolicy::capabilities(aterm_containment::current_mode());
    assert_eq!(caps.network, aterm_containment::NetworkCapability::Full);
    assert_eq!(caps.fs, aterm_containment::FsCapability::HomeReadWrite);
    assert_eq!(caps.process, aterm_containment::ProcessCapability::Full);
}
