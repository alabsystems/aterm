// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0
//

use super::*;

fn policy(enabled: bool) -> HarnessPolicy {
    let text = if enabled {
        "[harness]\napprove = \"all\"\n"
    } else {
        "[harness]\nenabled = false\n"
    };
    HarnessPolicy::read(text)
}

#[test]
fn the_newest_stamp_wins_in_either_order() {
    for older_first in [true, false] {
        let mut source = HarnessStampSource::new();
        let older = source.stamp(policy(true));
        let newer = source.stamp(policy(false));
        let mut cell = HarnessPolicyCell::new(policy(true));
        let (first, second) = if older_first {
            (older, newer)
        } else {
            (newer, older)
        };
        assert!(cell.offer(first));
        assert_eq!(cell.offer(second), older_first, "a stale stamp is refused");
        assert!(!cell.get().policy.enabled, "the newer read is in force");
    }
}
