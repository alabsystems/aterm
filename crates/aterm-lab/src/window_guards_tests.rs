// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use super::*;

#[test]
fn descriptor_roles_are_exact_and_do_not_carry_a_budget() {
    assert!(WindowGuards::arguments(&[3], None).is_ok());
    assert!(WindowGuards::arguments(&[3, 4], Some("original.claim")).is_ok());
    for (slots, claim) in [
        (&[3, 4][..], None),
        (&[3][..], Some("original.claim")),
        (&[3, 3][..], Some("original.claim")),
        (&[3, 4][..], Some("../other.claim")),
    ] {
        assert!(WindowGuards::arguments(slots, claim).is_err());
    }
}
