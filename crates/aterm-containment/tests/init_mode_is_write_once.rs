// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0
// Author: Andrew Yates

//! Integration test: the containment mode is write-once — a second `init_mode`
//! (an escalation to Master) is refused and the first mode stands.
//!
//! Fresh OnceLock (a fresh process: `crate::in_fresh_process`).

use aterm_containment::ContainmentMode;

#[test]
fn init_mode_is_write_once() {
    crate::in_fresh_process(module_path!(), "init_mode_is_write_once", || {
        aterm_containment::init_mode(ContainmentMode::Containment)
            .expect("first init should succeed");
        assert!(
            aterm_containment::init_mode(ContainmentMode::Master).is_err(),
            "escalation from Containment to Master must be rejected"
        );
        assert_eq!(
            aterm_containment::try_current_mode(),
            Some(ContainmentMode::Containment)
        );
    });
}
