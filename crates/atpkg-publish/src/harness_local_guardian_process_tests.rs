// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0

use super::*;
#[test]
fn group_physical_and_process_virtual_caps_are_independent_inclusive_bounds() {
    assert!(
        budget_reason(
            Usage {
                memory: 256 << 20,
                virtual_max: 1024 << 20
            },
            256,
            1024
        )
        .is_none()
    );
    assert!(
        budget_reason(
            Usage {
                memory: (256 << 20) + 1,
                virtual_max: 0
            },
            256,
            1024
        )
        .expect("physical cap")
        .contains("group")
    );
    assert!(
        budget_reason(
            Usage {
                memory: 0,
                virtual_max: (1024 << 20) + 1
            },
            256,
            1024
        )
        .expect("VM cap")
        .contains("virtual")
    );
}
