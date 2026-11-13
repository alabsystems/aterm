// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use crate::{App, WindowId};
use aterm_window::keyboard::{KeyCode, PhysicalKey};

fn arm_pending_overlap(app: &mut App) -> std::sync::mpsc::Receiver<()> {
    let (cancel, cancelled) = std::sync::mpsc::sync_channel(1);
    app.pending_update_handoff = Some(crate::PendingUpdateHandoff {
        park_at: std::time::Instant::now(),
        proof_ready_at: None,
        attempt_id: 1,
        nonce: None,
        live: Vec::new(),
        adoption: Vec::new(),
        child_pid: None,
        mode: crate::native_updater_service::ApplyMode::Immediate,
        apply_attempt: None,
        same_image: None,
        target_build: 0,
        target_commit: String::new(),
        layout: crate::restore::RestoreManifest::new(Vec::new()),
        carry_lone_lists: false,
        layout_digest: [0; 32],
        screen_digest: [0; 32],
        activity_epoch: app.update_handoff_activity_epoch,
        hold_serials: 0,
        carried_claims: Vec::new(),
        cancel,
        arbiter: crate::HandoffAttemptArbiter::new(),
        teardown: crate::DeferredHandoffTeardown::None,
        commit_drain_started: None,
        commit_flush: None,
        held_commit: None,
        revoked_by_activity: false,
    });
    cancelled
}

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
