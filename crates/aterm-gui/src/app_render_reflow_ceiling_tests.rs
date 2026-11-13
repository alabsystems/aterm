// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use super::{REFLOW_WORKER_MAX, reflow_worker_ceiling_for};

#[test]
fn the_ceiling_is_bounded_by_physical_cores_not_hyperthreads() {
    for (physical, logical, want) in [
        (Some(4), Some(8), 4),
        (Some(12), Some(12), REFLOW_WORKER_MAX),
        (Some(8), Some(2), 2),
        (None, Some(6), 6),
        (None, None, REFLOW_WORKER_MAX),
        (Some(0), Some(8), 8),
    ] {
        assert_eq!(
            reflow_worker_ceiling_for(physical, logical),
            want,
            "physical {physical:?}, logical {logical:?}"
        );
    }
}
