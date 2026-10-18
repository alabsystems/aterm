// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0
// Author: Andrew Yates

//! Integration test: fail-closed behavior when init_mode() is never called.
//!
//! Each test runs in a fresh process (`crate::in_fresh_process`), so the global
//! OnceLock has NOT been initialized. All gates should default to Containment mode
//! (maximally restrictive).

use aterm_containment::ContainmentMode;

#[test]
fn mode_or_containment_defaults_to_containment_when_uninit() {
    crate::in_fresh_process(
        module_path!(),
        "mode_or_containment_defaults_to_containment_when_uninit",
        || {
            // init_mode() has NOT been called in this fresh process.
            assert_eq!(aterm_containment::try_current_mode(), None);

            let mode = aterm_containment::mode_or_containment();
            assert_eq!(mode, ContainmentMode::Containment);
        },
    );
}
