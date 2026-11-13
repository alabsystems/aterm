// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use super::CpuObservation;

#[test]
fn unsupported_platform_is_not_reported_as_an_empty_pool() {
    assert_eq!(CpuObservation::read(None), CpuObservation::Unsupported);
}
