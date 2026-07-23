// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0
// Author: Andrew Yates

//! Integration test: verify `init_mode_from_env` rejects numeric bypass attempts.
//!
//! Fresh OnceLock (separate binary). An attacker might try numeric values that
//! match the `repr(u8)` encoding of `ContainmentMode`. These MUST be rejected
//! and MUST NOT initialize the global mode.
//!
//! Runs in its own test binary so `std::env::set_var` is safe: no other thread
//! in this process is reading the environment concurrently. This avoids the
//! Rust 2024 `unsafe { set_var }` data race UB that arose when multiple
//! `#[test]` functions in one binary mutated the process environment under
//! cargo's default parallel runner.

use aterm_containment::ContainmentMode;

#[test]
fn init_from_env_rejects_numeric_bypass_attempt() {
    // Attacker might try numeric values matching repr(u8) encoding.
    // SAFETY: Single-threaded test binary — no concurrent env reads.
    unsafe {
        std::env::set_var("ATERM_CONTAINMENT_MODE", "3");
    }

    let result = aterm_containment::init_mode_from_env(ContainmentMode::Containment);
    assert!(result.is_err(), "numeric values must be rejected");

    // Mode should still be uninitialized.
    assert_eq!(aterm_containment::try_current_mode(), None);
}
