// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use super::pressure_shed_budget;

#[test]
fn pressure_shed_budget_policy() {
    let mib = 1usize << 20;
    assert!(pressure_shed_budget(100 * mib, true) < pressure_shed_budget(100 * mib, false));
    assert_eq!(pressure_shed_budget(2 * mib, true), mib, "floor at 1 MiB");
    assert_eq!(
        pressure_shed_budget(0, false),
        mib,
        "floor even at a zero budget"
    );
    for &b in &[mib, 8 * mib, 512 * mib] {
        assert!(pressure_shed_budget(b, false) <= b);
        assert!(pressure_shed_budget(b, true) <= b);
    }
}
