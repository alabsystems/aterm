// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Test support: a pending update handoff in its overlap, for a test of what stands it down.

use crate::App;

/// Arm `app` with a pending handoff in its overlap, at its current activity epoch, and return
/// the channel its cancel is sent on ([`App::note_update_handoff_activity`]).
pub(crate) fn arm_pending_overlap(app: &mut App) -> std::sync::mpsc::Receiver<()> {
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
        table_flush: None,
        held_commit: None,
        revoked_by_activity: false,
    });
    cancelled
}
