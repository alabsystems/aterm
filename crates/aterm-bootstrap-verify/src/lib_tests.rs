// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use super::unix_now_fail_closed;
use std::time::{Duration, UNIX_EPOCH};

#[test]
fn unix_now_fails_closed_before_epoch_and_on_overflow() {
    assert_eq!(unix_now_fail_closed(UNIX_EPOCH), 0);
    assert_eq!(
        unix_now_fail_closed(UNIX_EPOCH + Duration::from_secs(1_785_801_600)),
        1_785_801_600
    );
    if let Some(pre_epoch) = UNIX_EPOCH.checked_sub(Duration::from_secs(1)) {
        assert_eq!(unix_now_fail_closed(pre_epoch), i64::MAX);
    }
    if let Some(overflow) = UNIX_EPOCH.checked_add(Duration::from_secs((i64::MAX as u64) + 1)) {
        assert_eq!(unix_now_fail_closed(overflow), i64::MAX);
    }
}
