// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0
// Author: Andrew Yates

//! Integration test: `init_mode_from_flag` uses the default when no flag was given.
//!
//! Fresh OnceLock (separate binary).

use aterm_containment::ContainmentMode;

#[test]
fn init_from_flag_uses_default_when_no_flag() {
    let mode = aterm_containment::init_mode_from_flag(None, ContainmentMode::User)
        .expect("init_mode_from_flag should succeed with default");

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
