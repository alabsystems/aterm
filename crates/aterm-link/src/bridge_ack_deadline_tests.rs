// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use super::*;

#[test]
fn a_timeout_fixture_changes_only_its_explicit_positive_deadline() {
    for input in [None, Some(""), Some("0"), Some("invalid")] {
        assert_eq!(ack_limit(input.map(std::ffi::OsStr::new)), ACK_DEADLINE);
    }
    let limit = ack_limit(Some(std::ffi::OsStr::new("250")));
    assert_eq!(limit, Duration::from_millis(250));
    assert_eq!(limit.saturating_sub(Duration::from_secs(1)), Duration::ZERO);
    assert_eq!(ACK_DEADLINE, Duration::from_secs(60));
}
