// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use super::successor_reads_fabric_carry;

#[test]
fn only_a_successor_at_least_as_new_holds_retirements_back() {
    assert!(successor_reads_fabric_carry(1_790_700_000, 1_790_607_299));
    assert!(successor_reads_fabric_carry(1_790_607_299, 1_790_607_299));
    assert!(!successor_reads_fabric_carry(1_790_585_188, 1_790_607_299));
}
