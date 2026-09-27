// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0
// Author: Andrew Yates

//! Integration test: `init_mode_from_flag` rejects an invalid mode string.
//!
//! Fresh OnceLock (separate binary). An invalid `--containment` value returns a
//! parse error WITHOUT initializing the global mode.

use aterm_containment::ContainmentMode;

#[test]
fn init_from_flag_rejects_invalid_value() {
    let result =
        aterm_containment::init_mode_from_flag(Some("ESCALATE_TO_ROOT"), ContainmentMode::Master);
    assert!(result.is_err(), "should reject invalid mode string");

    let msg = result.unwrap_err().to_string();
    assert!(
        msg.contains("invalid containment mode") && msg.contains("ESCALATE_TO_ROOT"),
        "error should say what was invalid: {msg}"
    );

    // OnceLock should NOT have been set — parse error is returned before init_mode.
    assert_eq!(
        aterm_containment::try_current_mode(),
        None,
        "global mode must not be initialized on parse error"
    );
}
