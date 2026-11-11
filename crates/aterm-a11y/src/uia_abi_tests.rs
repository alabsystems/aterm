// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use super::*;

#[test]
fn nullable_output_is_initialized_before_either_success_or_refusal() {
    let mut sentinel = 7u8;
    let mut out = &mut sentinel as *mut u8;
    assert!(unsafe { initialize_nullable_out(&mut out) });
    assert!(
        out.is_null(),
        "an absent success must expose null, not caller bytes"
    );
    out = &mut sentinel;
    assert_eq!(
        unsafe { *out },
        7,
        "clearing the slot does not release its old value"
    );
    assert!(!unsafe { initialize_nullable_out::<u8>(std::ptr::null_mut()) });
}

#[test]
fn ref_count_drops_once_at_zero_and_pins_sticky_at_saturation() {
    let count = RefCount::one();
    assert_eq!(count.retain(), 2);
    assert_eq!(count.release(), (1, false));
    assert_eq!(count.release(), (0, true));
    assert_eq!(count.release(), (0, false));

    let near_cap = RefCount::from_raw(SATURATED_REFS - 1);
    assert_eq!(near_cap.retain(), SATURATED_REFS);
    assert_eq!(near_cap.retain(), SATURATED_REFS);
    assert_eq!(near_cap.release(), (SATURATED_REFS, false));
}
