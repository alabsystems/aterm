// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0
// Author: Andrew Yates

//! Integration test: `init_mode_from_flag` rejects a `--containment` value that
//! is not a mode — empty, a `repr(u8)` number, or junk — WITHOUT initializing
//! the global mode (the launcher then fails closed).
//!
//! Fresh OnceLock (a fresh process: `crate::in_fresh_process`).

use aterm_containment::ContainmentMode;

#[test]
fn init_from_flag_rejects_unparsable_values() {
    crate::in_fresh_process(
        module_path!(),
        "init_from_flag_rejects_unparsable_values",
        || {
            for value in ["", "2", "ESCALATE_TO_ROOT"] {
                let result =
                    aterm_containment::init_mode_from_flag(Some(value), ContainmentMode::User);
                let err = result.expect_err("unparsable value must be rejected");
                assert_eq!(
                    err.to_string(),
                    format!(
                        "invalid containment mode: {value:?} (expected: user, safety, containment)"
                    ),
                    "{value:?}: Display must format without duplicating the prefix"
                );
                assert_eq!(
                    aterm_containment::try_current_mode(),
                    None,
                    "{value:?}: global mode must not be initialized on parse error"
                );
            }
        },
    );
}
