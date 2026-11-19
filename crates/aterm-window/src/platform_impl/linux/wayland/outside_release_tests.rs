// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0
//
// Added by the aterm project in 2026 to its copy of winit 0.30.13 to
// exercise Wayland tab-drop event ordering (see the repository NOTICE).

use super::*;

#[test]
fn enter_and_release_in_one_dispatch_keep_the_destination_until_the_release_callback() {
    let mut drops = OutsideReleases::default();
    let source = WindowId::from(1);
    let target = WindowId::from(2);
    let release = drops.released(7, 123, source);
    drops.entered(7, target, PhysicalPosition::new(23.5, 41.0));
    drops.stop_observing(7);
    // A later click can be in the same native queue. It does not own the earlier event.
    let next = drops.released(7, 129, target);
    drops.entered(7, source, PhysicalPosition::new(5.0, 6.0));
    assert_eq!(drops.pending[&release].target, Some((target, PhysicalPosition::new(23.5, 41.0))));
    assert_eq!(drops.pending[&next].target, Some((source, PhysicalPosition::new(5.0, 6.0))));
    drops.dispatched(release);
    drops.dispatched(next);
    assert!(drops.is_empty(), "unclaimed releases retain no drag state");
}

#[test]
fn an_outside_release_without_a_client_focus_stays_outside() {
    let mut drops = OutsideReleases::default();
    let release = drops.released(7, 10, WindowId::from(1));
    drops.entered(9, WindowId::from(2), PhysicalPosition::new(1.0, 2.0));
    assert_eq!(drops.pending[&release].target, None, "another seat entered");
    drops.stop_observing(7);
    drops.entered(7, WindowId::from(3), PhysicalPosition::new(9.0, 8.0));
    assert_eq!(drops.pending[&release].target, None, "a later pointer action entered");
}
