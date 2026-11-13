// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

#[test]
fn any_one_of_the_three_display_variables_counts_as_a_display() {
    for var in ["WAYLAND_DISPLAY", "WAYLAND_SOCKET", "DISPLAY"] {
        assert!(
            super::display_is_reachable_from(|v| (v == var).then(|| ":0".to_string())),
            "{var} alone must be enough to keep the real backend"
        );
    }
}

#[test]
fn an_empty_variable_is_not_a_display() {
    assert!(!super::display_is_reachable_from(|_| Some(String::new())));
    assert!(!super::display_is_reachable_from(|_| None));
}
