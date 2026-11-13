// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use super::*;

/// The verdict agrees only where the run read what aterm assumes. NEGATIVE CONTROL: the
/// other reading disagrees, either way round.
#[test]
fn the_verdict_holds_the_run_to_what_aterm_assumes() {
    assert_eq!(verdict(true, true), ("AGREES", 0));
    assert_eq!(verdict(false, false), ("AGREES", 0));
    assert_eq!(verdict(false, true), ("DISAGREES", 1), "CONTROL");
    assert_eq!(verdict(true, false), ("DISAGREES", 1), "CONTROL");
}
