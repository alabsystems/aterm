// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use super::*;

#[test]
fn a_fresh_plane_uses_its_fence_and_a_typed_plane_keeps_its_decay() {
    let used = std::cell::RefCell::new(Vec::new());
    for plane in [Plane::Fresh, Plane::AfterTyping] {
        settle_plane(
            plane,
            || {
                used.borrow_mut().push("fence");
                Ok(())
            },
            || used.borrow_mut().push("decay"),
        )
        .unwrap();
    }
    assert_eq!(*used.borrow(), ["fence", "decay"]);
    assert!(
        settle_plane(
            Plane::Fresh,
            || could_not_run("no evidence"),
            || panic!("a delay must not replace the fence")
        )
        .is_err()
    );
}

#[test]
fn plane_settle_needs_every_live_trail_component_dark() {
    let quiet = "trail ribbon_active=false sparks=0 glow_active=false pet_active=true licensed=100 spawns=100";
    assert_eq!(plane_dark(quiet), Ok(true));
    for (old, new) in [
        ("ribbon_active=false", "ribbon_active=true"),
        ("sparks=0", "sparks=1"),
        ("glow_active=false", "glow_active=true"),
    ] {
        assert_eq!(plane_dark(&quiet.replace(old, new)), Ok(false));
        assert!(plane_dark(&quiet.replace(old, "")).is_err());
    }
}
