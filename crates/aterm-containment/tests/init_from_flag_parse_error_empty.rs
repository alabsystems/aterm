// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0
// Author: Andrew Yates

//! Integration test: `init_mode_from_flag` rejects an empty `--containment` value.
//!
//! Fresh OnceLock (separate binary). An empty value returns a parse error
//! WITHOUT initializing the global mode (the launcher then fails closed).

use aterm_containment::ContainmentMode;

#[test]
fn init_from_flag_rejects_empty_string() {
    let result = aterm_containment::init_mode_from_flag(Some(""), ContainmentMode::Master);
    assert!(result.is_err(), "empty string must be rejected");
    assert_eq!(aterm_containment::try_current_mode(), None);
}
