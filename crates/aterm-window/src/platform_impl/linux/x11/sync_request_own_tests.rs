// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates
//
// Added by the aterm project in 2026 to its copy of winit 0.30.13
// (see the repository NOTICE).

use super::*;

#[test]
fn a_turn_holds_a_configure_or_a_queued_frame_until_the_cap() {
    let since = Instant::now();
    let configure = Pending { value: 7, wait: Wait::Configure, since };
    assert!(configure.held(since, || false), "waiting for its configure: held");
    let frame = Pending { wait: Wait::Frame, ..configure };
    assert!(frame.held(since, || true), "its frame with a redraw queued: held");
    assert!(!frame.held(since, || false), "its frame with nothing queued: answered");
    for p in [configure, frame] {
        assert!(!p.held(since + HOLD_CAP, || true), "{:?} at the cap: answered", p.wait);
    }
}
