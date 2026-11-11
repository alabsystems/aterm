// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0

use super::DevicePolicy;

#[test]
fn headless_selection_never_calls_the_window_fallback() {
    for listed in [Some(7), None] {
        assert_eq!(
            DevicePolicy::Headless.select(|| listed, || panic!("window fallback")),
            listed
        );
    }
}

#[test]
fn windowed_selection_keeps_the_listed_device_and_uses_only_a_needed_fallback() {
    let calls = std::cell::Cell::new(0);
    for (listed, expected, count) in [(Some(7), Some(7), 0), (None, Some(9), 1)] {
        let selected = DevicePolicy::Windowed.select(
            || listed,
            || {
                calls.set(calls.get() + 1);
                Some(9)
            },
        );
        assert_eq!(selected, expected);
        assert_eq!(calls.get(), count);
    }
    assert_eq!(DevicePolicy::Windowed.select(|| None::<u8>, || None), None);
}
