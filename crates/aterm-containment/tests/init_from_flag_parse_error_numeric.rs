// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0
// Author: Andrew Yates

//! Integration test: `init_mode_from_flag` rejects numeric bypass attempts.
//!
//! Fresh OnceLock (separate binary). An attacker might try numeric values that
//! match the `repr(u8)` encoding of `ContainmentMode`. These MUST be rejected
//! and MUST NOT initialize the global mode.

use aterm_containment::ContainmentMode;

#[test]
fn init_from_flag_rejects_numeric_bypass_attempt() {
    let result = aterm_containment::init_mode_from_flag(Some("3"), ContainmentMode::Containment);
    assert!(result.is_err(), "numeric values must be rejected");

    // Mode should still be uninitialized.
    assert_eq!(aterm_containment::try_current_mode(), None);
}
