// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0
// Author: Andrew Yates

//! Integration test: verify mode fallback when hotswap manifest is corrupt.
//!
//! When the manifest's containment_mode field is corrupted or missing,
//! `restore_containment_mode()` falls back to `init_mode_from_env(Containment)`.
//! This test verifies the fail-closed fallback contract. (#5520 AC3, #5603)

use aterm_containment::ContainmentMode;

/// Corrupt manifest mode string falls through to env var / default.
///
/// This mirrors the fallback path in restore_containment_mode():
/// parse fails → init_mode_from_env(Containment).
#[test]
fn corrupt_manifest_mode_falls_back_to_env_default() {
    // Ensure env var is NOT set (so default is used).
    unsafe {
        std::env::remove_var("ATERM_CONTAINMENT_MODE");
    }

    // Simulate corrupted manifest mode field.
    let manifest_mode: Option<String> = Some("CORRUPTED_BY_ATTACKER".to_string());

    // Parse attempt fails — this is the expected behavior.
    let parsed = manifest_mode
        .as_deref()
        .and_then(|s| s.parse::<ContainmentMode>().ok());
    assert!(parsed.is_none(), "corrupted string must not parse");

    // Fallback: init_mode_from_env with Containment default (fail-closed).
    let mode = aterm_containment::init_mode_from_env(ContainmentMode::Containment)
        .expect("fallback init should succeed");

    // With no env var set, falls back to Containment (most restrictive).
    assert_eq!(mode, ContainmentMode::Containment);
    assert_eq!(
        aterm_containment::current_mode(),
        ContainmentMode::Containment
    );
}
