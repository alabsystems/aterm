// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0
// Author: Andrew Yates

//! Integration test: verify `init_mode_from_env` rejects an empty string value.
//!
//! Fresh OnceLock (separate binary). An empty `ATERM_CONTAINMENT_MODE` value
//! should return a parse error WITHOUT initializing the global mode.
//!
//! Runs in its own test binary so `std::env::set_var` is safe: no other thread
//! in this process is reading the environment concurrently. This avoids the
//! Rust 2024 `unsafe { set_var }` data race UB that arose when multiple
//! `#[test]` functions in one binary mutated the process environment under
//! cargo's default parallel runner.

use aterm_containment::ContainmentMode;

#[test]
fn init_from_env_rejects_empty_string() {
    // SAFETY: Single-threaded test binary — no concurrent env reads.
    unsafe {
        std::env::set_var("ATERM_CONTAINMENT_MODE", "");
    }

    let result = aterm_containment::init_mode_from_env(ContainmentMode::Master);
    assert!(result.is_err(), "empty string must be rejected");
    assert_eq!(aterm_containment::try_current_mode(), None);
}
