// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0

use super::rfc3339_older_than;

#[test]
fn empty_and_malformed_stamps_are_never_stale() {
    assert!(!rfc3339_older_than("", 60));
    assert!(!rfc3339_older_than("never", 60));
    assert!(!rfc3339_older_than("-", 60));
}

#[test]
fn ancient_stamps_are_stale_and_future_stamps_are_not() {
    assert!(rfc3339_older_than("2001-01-01T00:00:00Z", 60));
    assert!(!rfc3339_older_than("2999-01-01T00:00:00Z", 60));
}
