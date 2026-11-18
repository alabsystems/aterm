// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use super::*;

/// Only another relaunch holding the machine's sweep defers a hand-back without counting; a sweep
/// lock the state directory cannot hold counts against the hand-back's patience as any wait does,
/// so a fault that never clears cannot keep it waiting while the tab lives; every other step ends
/// it.
#[test]
fn a_hand_back_defers_only_for_another_relaunchs_sweep() {
    assert_eq!(step_back("busy:another-sweep"), Back::Defer);
    for step in [
        "busy:state-unwritable",
        "busy:lock-unopenable",
        "wait:shell-prompt",
        "wait:unanswered",
        "wait:thread-locked",
    ] {
        assert_eq!(step_back(step), Back::Wait, "{step}");
    }
    for step in [
        "done:resumed",
        "ended:handled",
        "failed:relaunch:ERR",
        "refused:no-shell",
    ] {
        assert_eq!(step_back(step), Back::Over, "{step}");
    }
}
