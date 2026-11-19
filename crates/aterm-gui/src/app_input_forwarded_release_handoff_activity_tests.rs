// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use crate::handoff_test_support::arm_pending_overlap;
use crate::{App, WindowId};
use aterm_window::keyboard::{KeyCode, PhysicalKey};

#[test]
fn forwarded_release_is_tolerated_while_overlap_pending() {
    use aterm_types::keyboard::{Key as TKey, Modifiers};

    let mut app = App::headless_for_test();
    let wid = WindowId(0);
    let physical = PhysicalKey::Code(KeyCode::KeyA);
    app.note_forwarded_press_key(
        wid,
        physical,
        false,
        TKey::Character('a'),
        Modifiers::empty(),
        None,
        None,
    );
    let cancelled = arm_pending_overlap(&mut app);
    let epoch = app.update_handoff_activity_epoch;
    app.release_physical_press(wid, physical);
    assert!(
        !app.physical_press_owners.contains_key(&physical),
        "the forwarded episode is closed by its release"
    );
    assert_eq!(
        app.update_handoff_activity_epoch, epoch,
        "a key release must not bump the activity epoch while an overlap is pending"
    );
    assert!(
        cancelled.try_recv().is_err(),
        "a key release must not poke the pending overlap's cancel channel"
    );
}
