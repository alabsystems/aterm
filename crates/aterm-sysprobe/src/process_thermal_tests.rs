// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use super::*;
#[test]
fn raw_states_are_exact_and_optional_low_power_stays_unknown() {
    for raw in 0..=3 {
        let reading = decode(raw, None).unwrap();
        assert_eq!(reading.state.raw(), raw);
        assert_eq!(reading.low_power, None);
    }
    assert_eq!(decode(-1, Some(false)), None);
    assert_eq!(decode(4, Some(true)), None);
    assert_eq!(decode(0, Some(true)).unwrap().low_power, Some(true));
}
#[test]
#[cfg(target_os = "macos")]
fn native_scalar_getter_needs_no_gui() {
    let reading = process_thermal().expect("Foundation thermal state");
    assert!((0..=3).contains(&reading.state.raw()));
}
