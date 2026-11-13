// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use super::{State, sync_with};

/// A change Carbon refuses is not recorded, so the next sync asks again: a refused
/// enable is not held and a refused release keeps the hold; a settled state asks nothing.
#[test]
fn a_refused_change_is_not_held_and_the_next_sync_asks_again() {
    let mut s = State {
        desired: true,
        app_active: true,
        private_front: false,
        engaged: false,
    };
    assert_eq!(
        sync_with(&mut s, |want| {
            assert!(want);
            -25
        }),
        Err(-25)
    );
    assert!(!s.engaged, "a refused enable is not held");
    assert_eq!(
        sync_with(&mut s, |want| {
            assert!(want);
            0
        }),
        Ok(()),
        "asked again"
    );
    assert!(s.engaged);
    assert_eq!(
        sync_with(&mut s, |_| panic!("settled: nothing to ask")),
        Ok(())
    );
    s.desired = false;
    assert_eq!(
        sync_with(&mut s, |want| {
            assert!(!want);
            -1
        }),
        Err(-1)
    );
    assert!(s.engaged, "a refused disable keeps the hold");
}

/// A private document in front of a focused window engages secure input though the
/// person never asked for it, and releases it when it leaves the front or the app loses
/// focus.
/// NEGATIVE CONTROL: with `sync_with`'s wish reading `desired` alone, the first sync asks
/// nothing and `` a private front engages `` fails.
#[test]
fn a_private_front_engages_secure_input_while_focused() {
    let mut s = State {
        desired: false,
        app_active: true,
        private_front: true,
        engaged: false,
    };
    let mut asked = None;
    assert_eq!(
        sync_with(&mut s, |want| {
            asked = Some(want);
            0
        }),
        Ok(())
    );
    assert_eq!(asked, Some(true), "a private front engages");
    assert!(s.engaged);
    s.app_active = false;
    assert_eq!(sync_with(&mut s, i32::from), Ok(()));
    assert!(!s.engaged, "no focus, no hold");
    s.app_active = true;
    s.private_front = false;
    assert_eq!(
        sync_with(&mut s, |_| panic!(
            "neither wished nor private: nothing to ask"
        )),
        Ok(())
    );
}
