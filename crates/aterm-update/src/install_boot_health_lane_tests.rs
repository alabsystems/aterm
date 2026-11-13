// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0

use super::{BootHealthLane, boot_health_lane};

#[test]
fn only_a_handoff_targets_first_counted_launch_is_count_only() {
    assert_eq!(boot_health_lane(true, 0), BootHealthLane::CountOnly);
    assert_eq!(boot_health_lane(true, 1), BootHealthLane::CountOnly);
    assert_eq!(boot_health_lane(true, 2), BootHealthLane::FullProof);
    assert_eq!(boot_health_lane(false, 0), BootHealthLane::FullProof);
    assert_eq!(boot_health_lane(false, 1), BootHealthLane::FullProof);
}
