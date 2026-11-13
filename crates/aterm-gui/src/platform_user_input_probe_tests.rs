// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use super::user_input_age_is_recent;

#[test]
fn input_age_reducer_is_boundary_exact_and_fail_closed() {
    assert!(user_input_age_is_recent(0.0, 0.5));
    assert!(user_input_age_is_recent(0.5, 0.5));
    assert!(!user_input_age_is_recent(0.500_001, 0.5));
    assert!(user_input_age_is_recent(f64::NAN, 0.5));
    assert!(user_input_age_is_recent(-0.001, 0.5));
    assert!(!user_input_age_is_recent(f64::INFINITY, 0.5));
}

#[cfg(target_os = "macos")]
#[test]
fn hid_idle_input_probe_is_callable_off_the_appkit_main_thread() {
    let first = super::hid_idle::seconds_since_last_input();
    let _ = super::recent_user_input_event(std::time::Duration::from_millis(500));
    let second = super::hid_idle::seconds_since_last_input();
    assert_eq!(
        first.is_some(),
        second.is_some(),
        "the cached handles answer consistently"
    );
    if let (Some(a), Some(b)) = (first, second) {
        assert!(a >= 0.0 && b >= 0.0, "idle ages are non-negative: {a} {b}");
    }
}
