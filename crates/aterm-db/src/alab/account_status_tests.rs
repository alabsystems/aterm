// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use super::*;

/// Only a live row with no status, `pending` or `active` is usable; every hold word, and a word
/// this build does not know, holds it.
#[test]
fn only_a_live_pending_or_active_account_is_usable() {
    assert!(usable(true, None));
    assert!(usable(true, Some("pending")));
    assert!(usable(true, Some("active")));
    assert!(!usable(false, Some("active")));
    for held in [
        "paused",
        "broken",
        "banned",
        "suspended",
        "cancelled",
        "later",
    ] {
        assert!(!usable(true, Some(held)), "{held}");
    }
}
