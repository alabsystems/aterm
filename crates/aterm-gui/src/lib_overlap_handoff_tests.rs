// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use super::{App, WindowId, stub_session};

fn overlap_model_step(
    model: &aterm_spec::derive::Model,
    state: &mut aterm_spec::interp::State,
    action: &'static str,
) {
    let successors = model.successors(action, state);
    assert_eq!(
        successors.len(),
        1,
        "shipping arbiter step must map to one {action} successor: {state:?}"
    );
    *state = successors[0].clone();
    for invariant in &model.invariants {
        assert!(
            model.check_invariant(invariant.name, state),
            "shipping arbiter violates {}::{} after {action}: {state:?}",
            model.name,
            invariant.name,
        );
    }
}

fn overlap_model_at_proof_ready() -> (aterm_spec::derive::Model, aterm_spec::interp::State) {
    let model = aterm_models_misc::native_update_overlap_handoff_model();
    let mut state = model.init_state();
    for action in [
        "ParkParentReaders",
        "SpawnReaderlessChild",
        "ChildPaintsExactProof",
    ] {
        overlap_model_step(&model, &mut state, action);
    }
    (model, state)
}

/// A rolled-back park re-attaches no reader to a pane whose shell already
/// exited (round eight, finding 17): its reader saw EOF, so a new one
/// would replay the exit. NEGATIVE CONTROL: the same pane before its EOF
/// is matched (a test App has no proxy, so the scan reports it unattached).
#[test]
fn a_rollback_resume_never_reattaches_an_exited_pane() {
    let mut app = App::headless_for_test();
    let mut fds = [0i32; 2];
    assert_eq!(unsafe { libc::pipe(fds.as_mut_ptr()) }, 0, "pipe");
    let id = *app.pool.sessions.keys().next().expect("fixture session");
    app.pool.sessions.get_mut(&id).unwrap().session.master = fds[0];
    assert!(
        !app.attach_deferred_readers(),
        "a parked live pane is matched for re-attach"
    );
    let _ = app.pool.sessions[&id]
        .session
        .pty_closed_at
        .set(std::time::Instant::now());
    assert!(
        app.attach_deferred_readers(),
        "an exited pane is left without a reader"
    );
    app.pool.sessions.get_mut(&id).unwrap().session.master = -1;
    for fd in fds {
        aterm_pty::close_fd(fd);
    }
}

#[test]
fn handoff_arbiter_linearizes_commit_and_reject_interleavings() {
    use crate::{HandoffAttemptArbiter, HandoffAttemptPhase, HandoffReaperOwner};

    let commit_first = HandoffAttemptArbiter::new();
    let (model, mut commit_state) = overlap_model_at_proof_ready();
    overlap_model_step(&model, &mut commit_state, "ArmConcurrentRejectContender");
    assert!(commit_first.try_begin_commit());
    overlap_model_step(&model, &mut commit_state, "MainWinsCommitArbiter");
    assert!(!commit_first.try_begin_reject());
    overlap_model_step(&model, &mut commit_state, "WorkerLosesRejectRace");
    assert_eq!(commit_first.phase(), HandoffAttemptPhase::Committing);
    assert_eq!(commit_state["arbiter"], 1);
    assert_eq!(commit_first.reaper_owner(), HandoffReaperOwner::Unclaimed);

    let reject_first = HandoffAttemptArbiter::new();
    let (model, mut reject_state) = overlap_model_at_proof_ready();
    overlap_model_step(&model, &mut reject_state, "ArmConcurrentRejectContender");
    assert!(reject_first.try_begin_reject());
    overlap_model_step(&model, &mut reject_state, "WorkerWinsRejectArbiter");
    assert!(!reject_first.try_begin_commit());
    assert!(reject_first.claim_reaper(HandoffReaperOwner::Worker));
    overlap_model_step(&model, &mut reject_state, "KillRejectedChild");
    assert!(!reject_first.claim_reaper(HandoffReaperOwner::Emergency));
    assert!(reject_first.finish_reap(HandoffReaperOwner::Worker));
    overlap_model_step(&model, &mut reject_state, "ReapKilledChild");
    assert_eq!(reject_first.reaper_owner(), HandoffReaperOwner::Finished);
    assert_eq!(reject_state["child_reaped"], 1);

    let failed_commit = HandoffAttemptArbiter::new();
    let (model, mut failed_state) = overlap_model_at_proof_ready();
    assert!(failed_commit.try_begin_commit());
    overlap_model_step(&model, &mut failed_state, "MainWinsCommitArbiter");
    // The post-proof failure seam after Commit won the arbiter: a dead child makes the
    // Commit write return EPIPE and this transition runs. Nothing here reaps.
    assert!(failed_commit.commit_failed_to_rejecting());
    overlap_model_step(&model, &mut failed_state, "CommitWriteFails");
    assert_eq!(failed_commit.phase(), HandoffAttemptPhase::Rejecting);
    assert!(failed_commit.claim_reaper(HandoffReaperOwner::Emergency));
    overlap_model_step(&model, &mut failed_state, "KillRejectedChild");
    assert!(!failed_commit.claim_reaper(HandoffReaperOwner::Worker));
    assert!(failed_commit.finish_reap(HandoffReaperOwner::Emergency));
    overlap_model_step(&model, &mut failed_state, "ReapKilledChild");
    assert_eq!(failed_state["commit"], 0);
    assert_eq!(failed_state["parent_exited"], 0);
}

fn arm_outgoing_handoff(app: &mut App) -> std::sync::mpsc::Receiver<()> {
    let layout = app.capture_restore_manifest();
    let layout_digest = crate::seamless::layout_digest(&layout).expect("canonical layout");
    let live: Vec<_> = app
        .pool
        .iter()
        .map(|session| (session.id, session.master, session.pid))
        .collect();
    let (cancel, cancelled) = std::sync::mpsc::sync_channel(1);
    let arbiter = crate::HandoffAttemptArbiter::new();
    app.pending_update_handoff = Some(crate::PendingUpdateHandoff {
        park_at: std::time::Instant::now(),
        proof_ready_at: None,
        attempt_id: 7,
        nonce: None,
        live: live.clone(),
        adoption: live,
        child_pid: Some(77),
        mode: crate::native_updater_service::ApplyMode::Immediate,
        apply_attempt: None,
        same_image: None,
        target_build: crate::build_info::build_number().parse().unwrap_or(0),
        target_commit: crate::build_info::git_commit().to_string(),
        layout,
        layout_digest,
        screen_digest: [9; 32],
        activity_epoch: app.update_handoff_activity_epoch,
        hold_serials: app.hold_serials(),
        carried_claims: Vec::new(),
        cancel,
        arbiter,
        teardown: crate::DeferredHandoffTeardown::None,
        commit_drain_started: None,
        commit_flush: None,
        held_commit: None,
        revoked_by_activity: false,
    });
    cancelled
}

/// The outgoing process publishes the successor's title before Commit: parked for build
/// B with B staged, it publishes B's plain `v<version>`; a staged record for another
/// build or a same-image relaunch publishes nothing.
#[test]
fn the_outgoing_process_titles_the_bar_for_its_successor_before_commit() {
    let mut app = App::headless_for_test();
    app.push_stub_tab(WindowId(0), stub_session(app.next_session_id));
    let _cancelled = arm_outgoing_handoff(&mut app);
    let target = 1_790_278_596;
    app.pending_update_handoff
        .as_mut()
        .expect("armed")
        .target_build = target;
    app.relaunch = Some(crate::relaunch_notice::RelaunchNotice {
        build: target,
        version: "0.92.0".to_string(),
    });
    let title = app
        .publish_handoff_target_to_menu_bar()
        .expect("the successor is nameable");
    assert_eq!(title, "v0.92.0");

    app.relaunch = Some(crate::relaunch_notice::RelaunchNotice {
        build: target + 1,
        version: "0.93.0".to_string(),
    });
    assert_eq!(
        app.publish_handoff_target_to_menu_bar(),
        None,
        "a staged record for another build is not this target"
    );
    app.relaunch = Some(crate::relaunch_notice::RelaunchNotice {
        build: target,
        version: "0.92.0".to_string(),
    });
    app.pending_update_handoff
        .as_mut()
        .expect("armed")
        .same_image = Some(crate::app_update_handoff::SameImageHandoff::DebugSeam);
    assert_eq!(
        app.publish_handoff_target_to_menu_bar(),
        None,
        "a same-image relaunch is not a new version"
    );
    app.pending_update_handoff = None;
    assert_eq!(
        app.publish_handoff_target_to_menu_bar(),
        None,
        "nothing parked"
    );
}

/// Arm `app` as an incoming successor that owes its readiness proof under
/// `nonce` (layout digest `[7; 32]`, screen digest `[8; 32]`): the parent's
/// read end of the proof channel, its write end of the Commit pipe, and the
/// reader gate the Commit opens. Every end is close-on-exec, so a child
/// another test spawns holds none of them.
fn arm_incoming_proof(
    app: &mut App,
    nonce: &str,
) -> (
    std::fs::File,
    std::fs::File,
    crate::spawn::DeferredReaderGate,
) {
    use std::os::fd::FromRawFd as _;
    let (mut ready, mut commit) = ([0i32; 2], [0i32; 2]);
    // SAFETY: both arrays provide pipe(2)'s two writable descriptor slots.
    assert_eq!(unsafe { libc::pipe(ready.as_mut_ptr()) }, 0, "ready pipe");
    assert_eq!(unsafe { libc::pipe(commit.as_mut_ptr()) }, 0, "commit pipe");
    for fd in ready.into_iter().chain(commit) {
        aterm_pty::set_cloexec(fd, true).expect("close-on-exec");
    }
    // SAFETY: these four newly created descriptors have one owner each.
    let (ready_read, ready_write, commit_read, commit_write) = unsafe {
        (
            std::fs::File::from_raw_fd(ready[0]),
            std::os::fd::OwnedFd::from_raw_fd(ready[1]),
            std::os::fd::OwnedFd::from_raw_fd(commit[0]),
            std::fs::File::from_raw_fd(commit[1]),
        )
    };
    app.handoff_ready = Some(crate::seamless::ReadySignal::for_test(
        ready_write,
        nonce,
        [7; 32],
        [8; 32],
    ));
    app.handoff_commit = Some(crate::seamless::CommitReceiver::for_test(commit_read));
    let gate = crate::spawn::DeferredReaderGate::closed();
    app.handoff_reader_gate = Some(gate.clone());
    (ready_read, commit_write, gate)
}

/// Readiness gate and exact proof: an undrained restore queue holds the proof,
/// and so does unfinished control preparation; once both are done, exactly one
/// proof lands on the parent's read end and the write end is consumed.
#[test]
fn readiness_proof_fires_only_when_drained() {
    let mut app = App::headless_for_test();
    let nonce = "readiness-proof-test";
    let (mut ready_read, _commit_write, gate) = arm_incoming_proof(&mut app, nonce);
    app.pending_restore = Some(crate::restore::RestoreManifest::new(Vec::new()));
    app.maybe_signal_handoff_ready();
    assert!(
        app.handoff_ready.is_some(),
        "proof held while the restore queue is undrained"
    );
    app.pending_restore = None;
    let control_preparation = crate::control::ControlPreparation::default();
    app.handoff_control_preparation = Some(control_preparation.clone());
    app.maybe_signal_handoff_ready();
    assert!(
        app.handoff_ready.is_some(),
        "control preparation must hold the real proof"
    );
    assert!(!gate.is_released());
    control_preparation.finish(true);
    app.maybe_signal_handoff_ready();
    assert!(app.handoff_ready.is_none(), "fd consumed at signal time");
    let mut wire = Vec::new();
    std::io::Read::read_to_end(&mut ready_read, &mut wire).expect("the proof, then EOF");
    let wire: [u8; crate::seamless::READY_WIRE_LEN] = wire
        .try_into()
        .expect("exactly one proof, and the write end closed after it");
    assert_eq!(
        crate::seamless::AdoptionProof::from_wire(&wire).expect("a valid proof"),
        crate::seamless::adoption_proof(
            nonce,
            crate::build_info::build_number()
                .parse::<u64>()
                .unwrap_or(0),
            crate::build_info::git_commit(),
            &[7; 32],
            &[8; 32],
            &[],
        )
        .expect("empty-set proof")
    );
}

/// A failed capture barrier does not withdraw handoff readiness: the proof's paint
/// decision ([`super::handoff_paint_proven`], windowed arm) ignores a nulled
/// `last_present` on a window that already painted.
#[test]
fn a_failed_capture_barrier_does_not_withdraw_handoff_readiness() {
    use super::handoff_paint_proven as proven;
    let mut app = App::headless_for_test();
    let ws = app
        .windows
        .get_mut(&WindowId(0))
        .map(Box::as_mut)
        .expect("test window");
    assert!(
        !proven(false, true, std::iter::once(&*ws)),
        "control: a window that never presented has proven nothing"
    );
    ws.on_present_succeeded();
    assert!(
        proven(false, true, std::iter::once(&*ws)),
        "one real present proves the paint"
    );
    assert!(
        !proven(false, false, std::iter::once(&*ws)),
        "control: nothing is proven before window 0's first present"
    );
    // The capture barrier's first move, and then every attempt failed.
    ws.last_present = None;
    assert!(
        proven(false, true, std::iter::once(&*ws)),
        "a nulled RepaintKey is not an unpainted window"
    );
    // Control: a deferred reveal still holds the proof.
    ws.pending_reveal = Some(std::time::Instant::now());
    assert!(!proven(false, true, std::iter::once(&*ws)));
    assert!(
        proven(true, false, std::iter::once(&*ws)),
        "headless: the drained queues are the whole condition"
    );
}

/// Held shell re-keys are written by the commit waiter when Commit arrives, never at
/// the proof.
#[test]
fn held_rekeys_are_written_by_the_commit_and_never_before() {
    let mut app = App::headless_for_test();
    let dir = aterm_tempfile::Builder::new()
        .prefix("rk-commit")
        .tempdir()
        .expect("scratch");
    let key_file = dir.path().join("s-ab");
    let queue = std::sync::Arc::new(crate::shell_rekey::Deferred::holding());
    queue.hold(crate::shell_rekey::mint(&key_file, 1).0);
    app.session_factory.rekey_deferred = Some(queue);
    let (mut ready_read, mut commit_write, gate) = arm_incoming_proof(&mut app, "rekey-at-commit");
    let preparation = crate::control::ControlPreparation::default();
    preparation.finish(true);
    app.handoff_control_preparation = Some(preparation);
    app.maybe_signal_handoff_ready();
    assert!(app.handoff_ready.is_none(), "the proof fired");
    let mut wire = [0u8; crate::seamless::READY_WIRE_LEN];
    std::io::Read::read_exact(&mut ready_read, &mut wire).expect("the proof");
    let proof = crate::seamless::AdoptionProof::from_wire(&wire).expect("proof wire");
    assert!(!key_file.exists(), "no key before the Commit");
    std::io::Write::write_all(&mut commit_write, &proof.to_commit_wire()).expect("commit");
    aterm_spec::wait::until_within(
        aterm_spec::wait::MUST_HAPPEN,
        "the Commit opened the readers and wrote the held key",
        || (key_file.exists() && gate.is_released()).then_some(()),
    );
}

/// A preparation failure before the proof closes the parent's channel at once,
/// with no proof and the readers still shut: a control-resource failure, and a
/// reader preparation that fails (a test App has no event-loop proxy). The
/// close is synchronous: a successor that kept it open would cost the parent
/// its decision deadline, and here the read below would block for good.
#[test]
fn a_preparation_failure_closes_the_proof_channel_without_a_proof() {
    for cause in ["control resources", "reader preparation"] {
        let mut app = App::headless_for_test();
        let (mut ready_read, _commit_write, gate) = arm_incoming_proof(&mut app, cause);
        if cause == "control resources" {
            let preparation = crate::control::ControlPreparation::default();
            preparation.finish(false);
            app.handoff_control_preparation = Some(preparation);
        } else {
            app.pool
                .sessions
                .values_mut()
                .next()
                .expect("headless fixture session")
                .session
                .handoff_local_id = Some(0);
        }
        app.maybe_signal_handoff_ready();
        assert!(app.handoff_degraded, "{cause}");
        assert!(app.handoff_ready.is_none(), "{cause}");
        assert!(app.handoff_commit.is_none(), "{cause}");
        assert!(
            !gate.is_released(),
            "{cause}: a failure cannot authorize PTY consumption"
        );
        let mut byte = [0];
        assert_eq!(
            std::io::Read::read(&mut ready_read, &mut byte).expect("read"),
            0,
            "{cause}: the parent observes EOF, no proof"
        );
    }
}

/// Every tab close a pending handoff can be asked for — the native close of a
/// non-last tab, Cmd-W on the focused pane, and the control verb — is deferred
/// onto the handoff's teardown: no tab and no Session/PTY ownership edge goes
/// before the child is reaped, and the worker is told to abort. Once the
/// pending authority is cleared, the deferred close replays.
#[test]
fn pending_handoff_defers_every_tab_close_without_dropping_pty_ownership() {
    for closer in ["close_tab_at", "cmd-w", "control verb"] {
        let mut app = App::headless_for_test();
        app.push_stub_tab(WindowId(0), stub_session(app.next_session_id));
        let first = app.windows[&WindowId(0)].tab_set.tabs()[0].id;
        let (active, focus) = {
            let active = app.windows[&WindowId(0)]
                .tab_set
                .active()
                .expect("active tab");
            (active.id, active.focus)
        };
        let tabs_before = app.windows[&WindowId(0)].tab_set.len();
        let sessions_before: std::collections::BTreeSet<_> =
            app.pool.iter().map(|session| session.id).collect();
        let epoch_before = app.update_handoff_activity_epoch;
        let cancelled = arm_outgoing_handoff(&mut app);

        let deferred = match closer {
            "close_tab_at" => {
                assert!(!app.close_tab_at(WindowId(0), 0));
                Some(crate::DeferredHandoffMutation::CloseTab {
                    window: WindowId(0),
                    tab: first,
                })
            }
            "cmd-w" => {
                assert_eq!(app.close_active_tab(), None);
                Some(crate::DeferredHandoffMutation::CloseView {
                    window: WindowId(0),
                    tab: active,
                    view: focus,
                })
            }
            _ => {
                assert!(
                    app.apply_tab_cmd(crate::TabAction::Close(Some(0))).is_err(),
                    "a deferred close reports the deferral, not a state pair"
                );
                None
            }
        };
        assert_eq!(
            app.windows[&WindowId(0)].tab_set.len(),
            tabs_before,
            "{closer}"
        );
        assert_eq!(
            app.pool
                .iter()
                .map(|session| session.id)
                .collect::<std::collections::BTreeSet<_>>(),
            sessions_before,
            "{closer}: no Session/PTY ownership edge may detach before child reap"
        );
        assert!(
            cancelled.try_recv().is_ok(),
            "{closer}: the close requests the worker's abort"
        );
        if let Some(mutation) = deferred {
            assert!(
                matches!(
                    app.pending_update_handoff.as_ref().map(|pending| &pending.teardown),
                    Some(crate::DeferredHandoffTeardown::Mutations(mutations))
                        if mutations.contains(&mutation)
                ),
                "{closer}"
            );
        }
        if closer == "close_tab_at" {
            assert!(app.update_handoff_activity_epoch > epoch_before);
            // The completion boundary: only after the worker is gone and the
            // pending authority is cleared may the stable target be replayed.
            app.pending_update_handoff = None;
            assert!(!app.replay_deferred_handoff_tab_close(WindowId(0), first));
            assert_eq!(app.windows[&WindowId(0)].tab_set.len(), tabs_before - 1);
            assert_eq!(app.pool.iter().count(), sessions_before.len() - 1);
        }
    }
}

#[test]
fn pending_handoff_defers_last_and_non_last_window_teardown_then_quit_dominates() {
    let mut app = App::headless_for_test();
    let second = app.insert_logical_window(stub_session(app.next_session_id), 24, 80);
    let windows_before = app.windows.len();
    let pool_before = app.pool.iter().count();
    let cancelled = arm_outgoing_handoff(&mut app);

    assert_eq!(
        app.close_window_logical(WindowId(0)),
        crate::CloseOutcome::Stay
    );
    assert_eq!(app.close_window_logical(second), crate::CloseOutcome::Stay);
    assert_eq!(
        app.windows.len(),
        windows_before,
        "both windows remain live"
    );
    assert_eq!(
        app.pool.iter().count(),
        pool_before,
        "both PTYs remain owned"
    );
    assert!(cancelled.try_recv().is_ok());
    assert!(matches!(
        app.pending_update_handoff.as_ref().map(|pending| &pending.teardown),
        Some(crate::DeferredHandoffTeardown::Mutations(mutations))
            if mutations.contains(&crate::DeferredHandoffMutation::CloseWindow(WindowId(0)))
                && mutations.contains(&crate::DeferredHandoffMutation::CloseWindow(second))
    ));

    assert!(
        app.defer_pending_update_handoff_teardown(crate::DeferredHandoffTeardown::QuitRequested)
    );
    assert!(matches!(
        app.pending_update_handoff
            .as_ref()
            .map(|pending| &pending.teardown),
        Some(crate::DeferredHandoffTeardown::QuitRequested)
    ));
    assert_eq!(
        app.windows.len(),
        windows_before,
        "Cmd-Q is replay-only pre-reap"
    );
}

#[test]
fn automatic_appkit_termination_dominates_close_and_revokes_commit() {
    let mut app = App::headless_for_test();
    let epoch_before = app.update_handoff_activity_epoch;
    let _cancelled = arm_outgoing_handoff(&mut app);
    app.pending_update_handoff.as_mut().expect("pending").mode =
        crate::native_updater_service::ApplyMode::Automatic;

    assert!(app.defer_pending_update_handoff_teardown(
        crate::DeferredHandoffTeardown::NativeTerminate { generation: 41 }
    ));
    assert!(
        app.defer_pending_update_handoff_teardown(crate::DeferredHandoffTeardown::mutation(
            crate::DeferredHandoffMutation::CloseWindow(WindowId(0))
        ))
    );
    let pending = app.pending_update_handoff.as_ref().expect("pending");
    assert!(matches!(
        pending.teardown,
        crate::DeferredHandoffTeardown::NativeTerminate { generation: 41 }
    ));
    assert_ne!(
        app.update_handoff_activity_epoch, pending.activity_epoch,
        "native termination is explicit Commit revocation even without key activity"
    );
    assert!(app.update_handoff_activity_epoch > epoch_before);
    assert_eq!(
        app.windows.len(),
        1,
        "AppKit cannot tear down before reaper wake"
    );
}

#[test]
fn typing_buffers_through_a_pending_overlap_but_paste_still_revokes() {
    let mut app = App::headless_for_test();
    let cancelled = arm_outgoing_handoff(&mut app);
    let epoch = app.update_handoff_activity_epoch;

    // One keystroke's text: exactly one pass through the seam (the seam
    // itself is the proven single-delivery path), zero revocations.
    let _ = app.input(
        WindowId(0),
        crate::input::InputEvent::Text("hi".to_string()),
        crate::input::Source::Human,
    );
    assert_eq!(
        app.update_handoff_activity_epoch, epoch,
        "typing must keep the exact_activity Commit fact intact"
    );
    assert!(
        cancelled.try_recv().is_err(),
        "typing must not poke the worker cancel channel"
    );

    // The tolerated stamp updates only the quiet-epoch wall clock.
    let before = app.last_update_activity_at;
    app.note_update_handoff_tolerated_activity();
    assert!(app.last_update_activity_at >= before);
    assert_eq!(app.update_handoff_activity_epoch, epoch);
    assert!(cancelled.try_recv().is_err());

    // Pointer traffic buffers through too: hover, click, wheel and view scroll write no
    // lasting PTY bytes and touch nothing `commit_layout_topology` compares. Driven
    // through the real seam.
    for gesture in [
        crate::input::InputEvent::MouseMove {
            buttons: 3,
            row: 1,
            col: 2,
            mods: 0,
            side: aterm_core::selection::SelectionSide::Left,
            px_off: crate::input::PixelOffset { x: 0, y: 0 },
        },
        crate::input::InputEvent::Wheel {
            dir: aterm_types::mouse::WheelDir::Up,
            lines: 1,
            detent: true,
            row: 1,
            col: 2,
            mods: 0,
            px_off: crate::input::PixelOffset { x: 0, y: 0 },
        },
    ] {
        let _ = app.input(WindowId(0), gesture.clone(), crate::input::Source::Human);
        assert_eq!(
            app.update_handoff_activity_epoch, epoch,
            "{gesture:?} must keep the exact_activity Commit fact intact"
        );
        assert!(
            cancelled.try_recv().is_err(),
            "{gesture:?} must not poke the worker cancel channel"
        );
    }

    // Paste can straddle a bracketed-paste flip queued in the kernel:
    // still revoking.
    let _ = app.input(
        WindowId(0),
        crate::input::InputEvent::Paste("p".to_string(), crate::input::PasteFraming::AtDrain),
        crate::input::Source::Human,
    );
    assert!(
        app.update_handoff_activity_epoch > epoch,
        "paste must revoke the pending overlap"
    );
    assert!(
        cancelled.try_recv().is_ok(),
        "paste must poke the worker cancel channel"
    );

    // With NO pending overlap, the tolerated stamp degrades to the full
    // epoch bump so automatic admission pacing observes every event.
    app.pending_update_handoff = None;
    let epoch = app.update_handoff_activity_epoch;
    app.note_update_handoff_tolerated_activity();
    assert!(app.update_handoff_activity_epoch > epoch);
}

/// Round eight of the update audit, findings 724 and 752: once this successor
/// has COMMITTED (the Commit waiter released the reader gate, and the control
/// server serves), a wake that arrives before the Commit wake runs is HELD for
/// it, in arrival order, never dropped. A control client's verb keeps its reply
/// sender, so it is answered when the Commit wake runs it, not with `main-thread
/// reply dropped`; a reader's `Exit` leaves its pane open for the keystrokes
/// queued for it. Before the Commit every such wake is still swallowed, and the
/// readers' screen-moving wakes still run at once.
#[test]
fn a_wake_after_the_commit_waits_for_the_commit_wake() {
    use std::sync::mpsc::TryRecvError;
    let mut app = App::headless_for_test();
    let a = app
        .pool
        .iter()
        .next()
        .map(|session| session.id)
        .expect("headless fixture session");
    app.incoming_handoff_pending = true;
    let gate = crate::spawn::DeferredReaderGate::closed();
    app.handoff_reader_gate = Some(gate.clone());
    let typed = || {
        let (reply, answer) = std::sync::mpsc::channel();
        let wake = super::Wake::Input {
            mark: None,
            batch: vec![crate::input::InputEvent::KeySequence(b"x".to_vec())],
            src: crate::input::Source::Controller,
            reply: Some(reply),
            session: Some(a),
            fence: None,
        };
        (wake, answer)
    };
    let exit = || super::Wake::Exit {
        session: a,
        window: WindowId(0),
    };

    // Before the Commit the candidate swallows it, reply sender and all.
    let (wake, answer) = typed();
    assert!(app.admit_wake_before_commit(wake).is_none());
    assert!(matches!(answer.try_recv(), Err(TryRecvError::Disconnected)));
    assert!(app.admit_wake_before_commit(exit()).is_none());
    assert!(app.handoff_committed_wakes.is_empty(), "nothing is held");

    // Committed: the gate is released, the Commit wake has not run.
    gate.release();
    let (wake, answer) = typed();
    assert!(
        app.admit_wake_before_commit(wake).is_none(),
        "not run ahead of the Commit wake"
    );
    assert!(
        matches!(answer.try_recv(), Err(TryRecvError::Empty)),
        "its answer is still owed, not `main-thread reply dropped`"
    );
    assert!(app.admit_wake_before_commit(exit()).is_none());
    assert!(
        app.pool.get(a).is_some(),
        "the pane stays for the keys typed into it before Commit"
    );
    assert!(matches!(
        app.handoff_committed_wakes.as_slice(),
        [super::Wake::Input { .. }, super::Wake::Exit { .. }]
    ));
    // The readers' screen-moving wakes and the Commit wake itself run now.
    for runs in [
        super::Wake::Output {
            session: a,
            window: WindowId(0),
        },
        super::Wake::ActivateCommittedHandoff {
            expected: Vec::new(),
        },
    ] {
        assert!(app.admit_wake_before_commit(runs).is_some());
    }
    assert_eq!(app.handoff_committed_wakes.len(), 2);

    // Once the Commit wake ran, nothing is held or swallowed.
    app.incoming_handoff_pending = false;
    let (wake, _answer) = typed();
    assert!(app.admit_wake_before_commit(wake).is_some());
    assert_eq!(app.handoff_committed_wakes.len(), 2);
}

/// The pre-Commit input queue: bounded with the overflow counted; the kept prefix is
/// what was typed, in order (the newest is dropped); an incoherent queue disarms ambient
/// modifiers rather than replaying a stranded Super into Cmd-W.
#[test]
fn pre_commit_input_is_bounded_counted_and_cannot_strand_a_chord() {
    use super::{MAX_DEFERRED_HANDOFF_INPUT, ModifiersState};
    use aterm_window::event::WindowEvent;

    let _counters = crate::metrics::HANDOFF_INPUT_TEST_LOCK
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner);
    let (dropped_before, incoherent_before) = crate::metrics::handoff_input_counts();
    let mut app = App::headless_for_test();
    let winit_id = aterm_window::window::WindowId::from(0u64);
    app.incoming_handoff_pending = true;

    let typed = |n: usize| WindowEvent::Ime(aterm_window::event::Ime::Commit(format!("k{n}")));
    for n in 0..(MAX_DEFERRED_HANDOFF_INPUT + 88) {
        app.queue_pre_commit_input(winit_id, typed(n));
    }
    assert_eq!(
        app.handoff_deferred_input.len(),
        MAX_DEFERRED_HANDOFF_INPUT,
        "the queue is bounded"
    );
    assert_eq!(
        app.handoff_deferred_input_dropped, 88,
        "and every refusal past the bound is counted"
    );
    assert_eq!(
        crate::metrics::handoff_input_counts().0 - dropped_before,
        88,
        "…in `metrics` too, as each is refused (`input_dropped=`, gap #33)"
    );
    assert!(
        app.handoff_deferred_input_incoherent,
        "a drop breaks press/release pairing, so the stream is incoherent"
    );
    // The kept prefix is the first 512 in arrival order — the newest went.
    match (
        &app.handoff_deferred_input[0].1,
        &app.handoff_deferred_input[511].1,
    ) {
        (
            WindowEvent::Ime(aterm_window::event::Ime::Commit(first)),
            WindowEvent::Ime(aterm_window::event::Ime::Commit(last)),
        ) => {
            assert_eq!(first, "k0");
            assert_eq!(last, "k511");
        }
        other => panic!("queue holds the typed prefix in order, got {other:?}"),
    }
    // A non-deferrable event is not queued and costs no slot.
    let before = app.handoff_deferred_input.len();
    app.queue_pre_commit_input(winit_id, WindowEvent::RedrawRequested);
    assert_eq!(app.handoff_deferred_input.len(), before);

    // A window carrying Super when replay settles must drop it: its release may be one
    // of the refused events.
    app.windows
        .get_mut(&WindowId(0))
        .expect("headless window")
        .mods = ModifiersState::SUPER;
    let reported = app.settle_replayed_handoff_input();
    assert_eq!(
        reported, 88,
        "the drop count is reported once, then cleared"
    );
    assert!(
        app.windows.values().all(|ws| ws.mods.is_empty()),
        "an incoherent queue disarms the ambient modifiers; a stranded SUPER \
         would turn the next replayed `w` into Cmd-W against a live shell"
    );
    assert_eq!(app.handoff_deferred_input_dropped, 0, "counter is consumed");
    assert!(!app.handoff_deferred_input_incoherent, "flag is consumed");
    assert_eq!(
        crate::metrics::handoff_input_counts(),
        (dropped_before + 88, incoherent_before + 1),
        "the repair is counted once (`input_incoherent=`); the drops stay counted — \
         cumulative, never consumed by the settle"
    );

    // …and a COHERENT queue leaves a genuinely-held modifier alone.
    app.windows
        .get_mut(&WindowId(0))
        .expect("headless window")
        .mods = ModifiersState::SUPER;
    assert_eq!(app.settle_replayed_handoff_input(), 0);
    assert_eq!(
        app.windows[&WindowId(0)].mods,
        ModifiersState::SUPER,
        "nothing was dropped or reordered, so the snapshot is still evidence"
    );
    assert_eq!(
        crate::metrics::handoff_input_counts().1,
        incoherent_before + 1,
        "a coherent replay is not counted as a repair"
    );
}

/// A window losing focus with a chord already queued is marked: the replay disarms it
/// and counts the repair. A loss with nothing queued, or with input queued only for
/// another window, reorders nothing; an unnamed window's queued event counts as
/// possibly this one's.
#[test]
fn a_focus_loss_after_input_was_queued_for_the_window_marks_it_incoherent() {
    use super::ModifiersState;
    use aterm_window::event::WindowEvent;

    let _counters = crate::metrics::HANDOFF_INPUT_TEST_LOCK
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner);
    let incoherent_before = crate::metrics::handoff_input_counts().1;
    let mut app = App::headless_for_test();
    let this = aterm_window::window::WindowId::from(0u64);
    let other = aterm_window::window::WindowId::from(1u64);
    app.winit_to_window.insert(this, WindowId(0));
    app.winit_to_window.insert(other, WindowId(1));
    app.incoming_handoff_pending = true;

    // winit's synthetic `Focused(false)` at window creation, and the flips at
    // Commit's activation, arrive with nothing queued: they reorder nothing.
    app.on_focus(WindowId(0), false);
    app.on_focus(WindowId(0), true);
    app.on_focus(WindowId(0), false);
    assert!(
        !app.handoff_deferred_input_incoherent,
        "a focus loss with nothing queued ahead of it reorders nothing"
    );

    // Input queued for ANOTHER window: this window's loss reorders none of it.
    app.queue_pre_commit_input(other, WindowEvent::ModifiersChanged(Default::default()));
    app.on_focus(WindowId(0), false);
    assert!(
        !app.handoff_deferred_input_incoherent,
        "another window's queued input is untouched by this window's clear"
    );

    // A chord queued for this window, then the loss: out of order.
    app.queue_pre_commit_input(this, WindowEvent::ModifiersChanged(Default::default()));
    app.on_focus(WindowId(0), true);
    app.on_focus(WindowId(0), false);
    assert!(
        app.handoff_deferred_input_incoherent,
        "a loss after input was queued for the window runs out of order against it"
    );
    app.windows
        .get_mut(&WindowId(0))
        .expect("headless window")
        .mods = ModifiersState::SUPER;
    assert_eq!(
        app.settle_replayed_handoff_input(),
        0,
        "nothing was dropped"
    );
    assert!(
        app.windows[&WindowId(0)].mods.is_empty(),
        "the incoherent replay disarms the snapshot rather than trust it"
    );
    assert_eq!(
        crate::metrics::handoff_input_counts().1,
        incoherent_before + 1,
        "and the repair is counted (`input_incoherent=`)"
    );

    // A queued event whose window can no longer be named marks too.
    let mut app = App::headless_for_test();
    app.winit_to_window.insert(this, WindowId(0));
    app.incoming_handoff_pending = true;
    app.queue_pre_commit_input(
        aterm_window::window::WindowId::from(99u64),
        WindowEvent::ModifiersChanged(Default::default()),
    );
    app.on_focus(WindowId(0), false);
    assert!(
        app.handoff_deferred_input_incoherent,
        "unsure whose it is, the loss marks: the repair is the harmless direction"
    );

    // And nothing marks once the handoff has committed.
    let mut app = App::headless_for_test();
    app.winit_to_window.insert(this, WindowId(0));
    app.incoming_handoff_pending = false;
    app.handoff_deferred_input
        .push((this, WindowEvent::ModifiersChanged(Default::default())));
    app.on_focus(WindowId(0), false);
    assert!(
        !app.handoff_deferred_input_incoherent,
        "no handoff is pending"
    );
}

/// The automatic apply refuses to freeze inside a keystroke gap and opens after a pause.
/// Driven through the pure form; the App gate uses `any_os_window_focused`, so a
/// surfaceless window never holds the lane.
#[test]
fn the_keystroke_gate_holds_an_automatic_apply_until_the_typing_pauses() {
    use super::{AUTOMATIC_UPDATE_KEYSTROKE_GAP, hands_off_keys};
    use std::time::Instant;

    let now = Instant::now();
    assert!(
        hands_off_keys(true, None, now),
        "a process that has never seen a keystroke never holds the lane"
    );

    // Mid-word: the freeze must not start here.
    assert!(!hands_off_keys(true, Some(now), now));
    assert!(
        !hands_off_keys(true, Some(now), now + AUTOMATIC_UPDATE_KEYSTROKE_GAP / 2),
        "an inter-keystroke pause shorter than the gap is still typing"
    );

    // A pause opens it, and the update lands invisibly as it always did.
    assert!(hands_off_keys(
        true,
        Some(now),
        now + AUTOMATIC_UPDATE_KEYSTROKE_GAP
    ));

    // The INVERSION leg: the user is typing somewhere ELSE. Their keystrokes
    // are not ours to interrupt, so the lane proceeds — this is the leg that
    // would silently rot into "aterm never updates while any app is in use".
    assert!(
        hands_off_keys(false, Some(now), now),
        "no aterm window has focus, so no keystroke of ours is being interrupted"
    );

    // The SURFACELESS leg: a headless window seeds `focused: true` with no
    // OS surface; the App's gate does not count it as focus.
    let mut app = App::headless_for_test();
    assert!(
        app.windows.values().any(|ws| ws.focused) && !app.any_os_window_focused(),
        "precondition: a headless window believes it has focus and has no surface"
    );
    app.last_keystroke_at = Some(now);
    assert!(app.update_apply_hands_off_keys(now));
}

/// `PreferOutputGap` waits only on panes on screen (the active tab of a focused window);
/// a chatty background tab does not hold it.
#[test]
fn a_streaming_hidden_tab_does_not_hold_the_output_gap_phase() {
    use std::sync::atomic::Ordering;
    let mut app = App::headless_for_test();
    let wid = WindowId(0);
    let hidden = app
        .focused_session_id(wid)
        .expect("the first tab carries a session");
    app.push_stub_tab(wid, stub_session(app.next_session_id));
    let shown = app
        .focused_session_id(wid)
        .expect("the pushed tab is the active one");
    assert_ne!(hidden, shown);
    assert!(!app.is_visible_session(hidden) && app.is_visible_session(shown));

    let now = std::time::Instant::now();
    let now_ns = u64::try_from(now.saturating_duration_since(app.lat_epoch).as_nanos())
        .unwrap_or(u64::MAX)
        .max(1);
    let stamp = |app: &App, id: u64, ns: u64| {
        app.pool
            .get(id)
            .expect("pooled")
            .latest_output_activity_ns
            .store(ns, Ordering::Release);
    };
    let focused = |ws: &crate::WindowState| ws.focused;
    assert!(
        app.windows[&wid].focused,
        "a headless window is born focused"
    );

    // The hidden tab streams; the pane on screen is quiet.
    stamp(&app, hidden, now_ns);
    stamp(&app, shown, 0);
    assert!(
        app.automatic_update_output_quiet_where(now, focused),
        "output nobody can see must not hold the output-gap phase"
    );

    // Negative control: the same stream in the shown pane holds it.
    stamp(&app, shown, now_ns);
    assert!(!app.automatic_update_output_quiet_where(now, focused));

    // An unfocused window's active tab is not being watched either.
    app.windows.get_mut(&wid).expect("window").focused = false;
    assert!(app.automatic_update_output_quiet_where(now, focused));

    // The live gate requires a real OS surface, so a headless window never
    // holds the phase however much it streams.
    app.windows.get_mut(&wid).expect("window").focused = true;
    assert!(app.automatic_update_output_quiet(now));
}

/// While a consent warm-up is mid-gesture the automatic apply stands off in every
/// ladder phase, `Land` included, until `[privacy] warmup_hold_ms`. Manual applies are
/// never held.
#[test]
fn a_live_consent_warm_up_holds_the_automatic_apply_but_only_to_its_cap() {
    use crate::native_update_auto_intent::{ApplyPhase, automatic_park_refusal};
    use crate::native_updater_service::ApplyMode;
    use std::time::{Duration, Instant};

    let mut app = App::headless_for_test();
    let now = Instant::now();
    assert!(
        !app.update_apply_warmup_holds(now),
        "precondition: nothing holds the lane"
    );

    let cap = Duration::from_millis(120_000);
    app.consent_warmup.arm_hold_for_test(now, cap);
    assert!(
        app.update_apply_warmup_holds(now),
        "an owner-initiated warm-up is mid-gesture"
    );
    let facts = app.automatic_activity_facts(ApplyMode::AutomaticPastGrace, now);
    assert!(facts.consent_warmup);
    assert!(
        automatic_park_refusal(ApplyPhase::Land, facts).is_some(),
        "the ladder's last phase must not land on the dialog"
    );
    assert!(
        !app.automatic_activity_facts(ApplyMode::Immediate, now)
            .consent_warmup,
        "an explicit apply is never held"
    );

    // The ordering leg. A system consent dialog takes focus away from every
    // aterm window, which is exactly the case the keystroke gate answers
    // `true` for — so the hold is its own fact, not a part of that gate, or
    // it would evaporate the instant the dialog it exists for appeared.
    for ws in app.windows.values_mut() {
        ws.focused = false;
    }
    assert!(
        app.update_apply_warmup_holds(now),
        "losing focus to the dialog must not end the hold"
    );

    // It cannot pin a build.
    assert!(
        !app.update_apply_warmup_holds(now + cap),
        "the hold expires at its cap"
    );
    assert!(
        !app.update_apply_warmup_holds(now + cap + Duration::from_secs(3600)),
        "an hour past the cap the build is still not pinned"
    );
}

/// A headless instance has no Security panel to have gestured from, and a
/// warm-up is a deliberate consent prompt — so the gesture refuses, and
/// every query answers without a worker, a syscall or a hold.
#[test]
fn a_headless_instance_never_starts_a_consent_warm_up() {
    use std::time::Instant;

    let mut app = App::headless_for_test();
    assert_eq!(
        app.begin_consent_warmup(),
        super::consent_warmup::StartOutcome::Refused
    );
    assert!(!app.consent_warmup_live());
    assert!(app.consent_warmup_rows().is_empty());
    assert_eq!(app.consent_warmup_last_pass_ms(), None);
    assert!(
        !app.consent_warmup.holds_automatic_apply(Instant::now()),
        "nothing started, so nothing holds"
    );
}

/// The access card never reaches a headless instance: the lifecycle ticks without a
/// worker, row or leaving `Idle`; a need is not counted; a late verdict posts nothing.
#[test]
fn a_headless_instance_never_offers_the_macos_access_card() {
    use super::consent_card::{CardPhase, NeedCause};
    use super::message_reporters::KEY_FILE_ACCESS;
    use std::time::Instant;

    let mut app = App::headless_for_test();
    assert!(
        app.consent_card_config.is_none(),
        "a test instance has nowhere to write a marker"
    );
    app.tick_macos_access_card(Instant::now());
    assert_eq!(app.consent_card.phase(), CardPhase::Idle);
    assert!(
        app.messages.live_by_key(KEY_FILE_ACCESS).is_none(),
        "no row on a headless instance"
    );
    // A need on a headless instance is not even counted.
    app.note_fda_need(
        NeedCause::ProtectedRead { session: Some(1) },
        Instant::now(),
    );
    assert_eq!(app.consent_card_host.needs, 0);
    assert_eq!(app.consent_card.phase(), CardPhase::Idle);
    app.decide_macos_access_card(crate::consent_card::PendingDecision::launch(None));
    assert_eq!(
        app.consent_card.phase(),
        CardPhase::Idle,
        "a stray verdict is stale"
    );
    assert!(app.messages.live_by_key(KEY_FILE_ACCESS).is_none());
    // The only headless path that reaches the decision code: a verdict
    // in `Deciding` settles, and posts nothing.
    assert!(app.consent_card.begin_deciding(Instant::now()));
    app.decide_macos_access_card(crate::consent_card::PendingDecision::launch(None));
    assert_eq!(app.consent_card.phase(), CardPhase::Settled);
    assert!(app.messages.live_by_key(KEY_FILE_ACCESS).is_none());
}

/// A windowed-looking `App` for the access card's lifecycle, without a
/// window: headless-constructed — inert probe and gesture arms, so no
/// `tccd` and no `NSWorkspace` (the live Settings arm refuses off the
/// main thread, which is where a test runs) — with `headless` flipped so
/// the tick runs its real branches, and the cached probe reading denied.
fn access_card_windowed() -> App {
    let mut app = App::headless_for_test();
    app.headless = false;
    app.consent = crate::consent_state::ConsentState::with_cached_probe_for_test(
        aterm_containment::consent::FdaProbe {
            state: aterm_containment::consent::FdaState::Denied,
            label: aterm_containment::consent::ProbeLabel::OpenEperm,
        },
    );
    app
}

/// The digest entries of the question (`privacy.fda.needed` / `.regressed`).
fn fda_entries(app: &App) -> Vec<aterm_messages::DigestEntry> {
    app.messages
        .digest()
        .entries
        .into_iter()
        .filter(|e| e.key.starts_with("privacy.fda."))
        .collect()
}

/// The launch-time reassert rebuilds its words without the raised cause, so
/// it must leave an open entry alone: a refused read keeps its own title.
#[test]
fn the_launch_reassert_keeps_an_open_entrys_words() {
    use super::consent_card::{FDA_WHAT_DENIED, NeedCause, NeedWords};
    let mut app = App::headless_for_test();
    app.except(&super::message_reporters::file_access_entry(&NeedWords {
        cause: NeedCause::ProtectedRead { session: None },
        tab_number: None,
        regressed: false,
    }));
    app.reassert_fda_need(&NeedWords {
        cause: NeedCause::PromptObserved {
            service: None,
            session: None,
        },
        tab_number: None,
        regressed: false,
    });
    let entries = fda_entries(&app);
    assert_eq!(entries.len(), 1, "{entries:?}");
    assert_eq!(entries[0].title, FDA_WHAT_DENIED);
}

/// Drive a due card through the park's tick at `now`: the question is an
/// ENTRY of the digest ("To look at"), never a band row, and the card
/// watches the probe for the grant. Returns the entry's id.
fn access_card_posted(app: &mut App, now: std::time::Instant) -> aterm_messages::MessageId {
    use super::consent_card::{CardPhase, FDA_WHAT_DENIED, Verdict};
    assert!(app.consent_card.begin_deciding(now));
    assert!(app.consent_card.on_decided(&Verdict::Offer, now));
    app.tick_macos_access_card(now);
    let entries = fda_entries(app);
    assert_eq!(
        entries.len(),
        1,
        "the question is in the digest: {entries:?}"
    );
    let entry = &entries[0];
    assert_eq!(entry.key, "privacy.fda.needed");
    assert_eq!(
        entry.title, FDA_WHAT_DENIED,
        "no words kept: a refused read"
    );
    assert!(
        app.messages
            .live_by_key(super::message_reporters::KEY_FILE_ACCESS)
            .is_none(),
        "and never a band row"
    );
    assert_eq!(
        app.consent_card_message, None,
        "an entry has no id to watch"
    );
    assert_eq!(app.consent_card.phase(), CardPhase::Watching { since: now });
    entry.id
}

/// THE ACCESS QUESTION'S LIFECYCLE (design §3.10, R15; design-exceptions
/// §8.2): the verdict and the grant — the two paths with no platform gate
/// — hold on every host; on macOS the park raises the question ONCE, as
/// ONE ENTRY of the digest keyed `privacy.fda.needed` — never a band row,
/// so it costs the terminal nothing — and the owner's one dismissal
/// (`Dismiss all`) files it in the log, where it stays quiet for its span
/// however often the card asks again. It has no patience to run out (an
/// hour on it still stands; a row folded on its ten minutes and settled
/// the card), so the unanswered path is the owner's dismissal alone.
///
/// THE PARK POINT IS macOS-ONLY: `tick_macos_access_card` returns at its
/// FIRST line on every other target, because Full Disk Access is a TCC
/// concept (`macos_access_card_stays_inert_off_macos`).
#[test]
fn the_access_entry_is_raised_once_watches_the_probe_and_is_dismissed_once() {
    use super::consent_card::{CardPhase, NeedCause, NeedWords};
    use super::message_reporters::KEY_FILE_ACCESS;
    use aterm_messages::{Message, Severity, tags};
    use std::time::{Duration, Instant};

    let t0 = Instant::now();

    // EVERY HOST: a verdict in DECIDING against the inert arm. The cached
    // probe reads the planted denial, but the signing identity is cold, so
    // a grant would be keyed to `dr=unknown` and would not survive a
    // rebuild — quiet, nothing raised.
    let mut app = access_card_windowed();
    let panel = app.consent_panel_facts();
    assert_eq!(panel.fda, aterm_containment::consent::FdaState::Denied);
    assert!(
        !panel.dr.grant_stable(),
        "a cold identity promises no durable grant: {:?}",
        panel.dr
    );
    assert!(app.consent_card.begin_deciding(Instant::now()));
    app.decide_macos_access_card(crate::consent_card::PendingDecision::launch(None));
    assert_eq!(app.consent_card.phase(), CardPhase::Settled);
    assert!(fda_entries(&app).is_empty());
    assert!(app.messages.live_by_key(KEY_FILE_ACCESS).is_none());

    // The park point is macOS-only: what every other host's early return
    // owes is `macos_access_card_stays_inert_off_macos`'s.
    if !cfg!(target_os = "macos") {
        return;
    }

    // DUE: raised ONCE, and it costs the band nothing — a row already
    // there stays the top row, no row is wanted for the question, and a
    // later tick raises nothing more.
    let mut app = access_card_windowed();
    let other = app.post_message(Message::new(tags::SYSTEM, Severity::Warn, "already here"));
    let id = access_card_posted(&mut app, t0);
    assert_eq!(
        app.messages.on_glass().next().map(|l| l.id),
        Some(other),
        "the question is no row: the row that was there is still the top one"
    );
    assert_eq!(app.messages.wanted_rows(), 1, "only the row that was there");
    let entry = app.messages.digest_entry(id).expect("open");
    assert_eq!(entry.severity, Severity::Info, "a look, never an alarm");
    assert_eq!(
        entry.actions,
        [aterm_messages::Intent::OpenSystemPane {
            pane: super::message_reporters::PANE_FULL_DISK_ACCESS.to_string()
        }],
        "one button, and no `Not now`: the dismissal is the decline"
    );
    app.tick_macos_access_card(t0 + Duration::from_secs(1));
    assert_eq!(fda_entries(&app).len(), 1, "raised once");

    // IT DOES NOT FOLD: an hour past the row's old ten minutes it stands,
    // and the card is still watching for the grant.
    let hour = Instant::now() + Duration::from_secs(3_600);
    app.settle_messages(hour);
    assert_eq!(fda_entries(&app).len(), 1, "no patience to run out");
    assert!(matches!(
        app.consent_card.phase(),
        CardPhase::Watching { .. }
    ));
    assert!(
        app.consent_card_host.record.asked_at.is_some(),
        "raising the question recorded that it was asked"
    );

    // THE OWNER'S ONE DISMISSAL files it in the log; the card's asking
    // again (a fresh need, an update's launch check) raises nothing while
    // the filing holds.
    assert_eq!(app.dismiss_exceptions(), 1);
    assert!(fda_entries(&app).is_empty());
    assert!(matches!(
        app.messages
            .log()
            .get(id)
            .and_then(aterm_messages::LogRecord::retired),
        Some(aterm_messages::Retired::Dismissed)
    ));
    let words = NeedWords {
        cause: NeedCause::ProtectedRead { session: None },
        tab_number: None,
        regressed: false,
    };
    app.raise_fda_need(&words, t0);
    app.reassert_fda_need(&words);
    assert!(
        fda_entries(&app).is_empty(),
        "a fact the owner filed stays filed"
    );

    // …but a REGRESSION (a verified grant later reads off) is another
    // fact: news once.
    app.raise_fda_need(
        &NeedWords {
            regressed: true,
            ..words
        },
        t0,
    );
    let regressed = fda_entries(&app);
    assert_eq!(regressed.len(), 1);
    assert_eq!(regressed[0].key, "privacy.fda.regressed");

    // The record learns that the question was asked: the cache at once (above),
    // the file off the UI thread.
    let mut app = access_card_windowed();
    let record = aterm_tempfile::Builder::new()
        .prefix("aterm-access-card")
        .tempdir()
        .expect("scratch");
    app.consent_card_config = Some(record.path().join("aterm.toml"));
    access_card_posted(&mut app, t0);
    let asked = app.consent_card_host.record.asked_at;
    assert!(asked.is_some());
    aterm_spec::wait::until_within(
        aterm_spec::wait::MUST_HAPPEN,
        "the record file learns asked_at",
        || {
            (super::consent_card::read_record(app.consent_card_config.as_deref()).asked_at == asked)
                .then_some(())
        },
    );
}

/// OPEN SETTINGS (R16), ON THE ENTRY (design-exceptions §8.2): the button
/// opens the pane through the instance's gesture arm (it refuses off the
/// main thread, so the press reports the refusal) and the press is the
/// owner's ANSWER — `Answered`, in the log, "Open Settings" — not the end
/// of the question: a pane opened is not a switch turned on. The `opened`
/// marker and `opened_at` are recorded, the card keeps watching, a fresh
/// need is not raised while the answer holds, and ONLY the verified grant
/// ends it (`Resolved`, and a `Full Disk Access granted` record).
#[test]
#[cfg(target_os = "macos")]
fn open_settings_answers_the_entry_and_only_a_verified_grant_ends_it() {
    use super::consent_card::{CardPhase, GRANTED_CAPTION, Marker, NeedCause, NeedWords};
    use aterm_messages::{LogRecord, Retired};
    use std::time::{Duration, Instant};

    let t0 = Instant::now();
    let wid = WindowId(0);
    let mut app = access_card_windowed();
    let id = access_card_posted(&mut app, t0);
    let outcome = app.perform_message_act(wid, id.raw(), 0);
    assert!(
        matches!(
            outcome,
            crate::native_app::MessageActOutcome::Refused { .. }
        ),
        "the arm refuses off the main thread"
    );
    // ANSWERED, not cleared: filed with the button's label.
    assert!(fda_entries(&app).is_empty(), "the answer files the entry");
    assert!(
        matches!(
            app.messages.log().get(id).and_then(LogRecord::retired),
            Some(Retired::Answered { label }) if label == "Open Settings"
        ),
        "{:?}",
        app.messages.log().get(id).and_then(LogRecord::retired)
    );
    assert!(!crate::macos_access_refresh_tests::grant_recorded(&app));
    // The press was recorded for a later process, and the card watches on.
    assert!(app.consent_card_host.record.opened_at.is_some());
    assert_eq!(app.consent_card_host.marker, Some(Marker::Opened));
    assert!(matches!(
        app.consent_card.phase(),
        CardPhase::Watching { .. }
    ));
    // A fresh need inside the answer's span is quiet.
    let words = NeedWords {
        cause: NeedCause::ProtectedRead { session: None },
        tab_number: None,
        regressed: false,
    };
    app.raise_fda_need(&words, t0);
    assert!(fda_entries(&app).is_empty(), "answered: no second entry");
    // The owner turns the switch on: the window's own probe reads
    // `granted`, the watch notices with nothing further pressed.
    app.consent = crate::consent_state::ConsentState::with_cached_probe_for_test(
        aterm_containment::consent::FdaProbe {
            state: aterm_containment::consent::FdaState::Granted,
            label: aterm_containment::consent::ProbeLabel::OpenOk,
        },
    );
    app.tick_macos_access_card(t0 + Duration::from_secs(30));
    assert_eq!(app.consent_card.phase(), CardPhase::Settled);
    assert!(crate::macos_access_refresh_tests::grant_recorded(&app));
    assert_eq!(
        app.messages
            .log()
            .records()
            .next_back()
            .map(|rec| rec.title.as_str()),
        Some(GRANTED_CAPTION)
    );
}

/// THE GRANT ENDS THE ENTRY (R17): whichever cause it wears — the need or
/// a regression — the observed grant takes it out of the digest
/// (`resolved`, in the log), and the grant itself is a RECORD: Settings ▸
/// Messages and messages.log keep it, the glass does not (the owner's
/// attention rule). The card settles.
#[test]
#[cfg(target_os = "macos")]
fn a_grant_ends_the_entry_and_is_a_record() {
    use super::consent_card::{CardPhase, GRANTED_CAPTION};
    use super::message_reporters::KEY_FILE_ACCESS;
    use aterm_messages::{LogRecord, Outcome, Retired};
    use std::time::{Duration, Instant};

    let t0 = Instant::now();
    let mut app = access_card_windowed();
    let id = access_card_posted(&mut app, t0);
    app.consent = crate::consent_state::ConsentState::with_cached_probe_for_test(
        aterm_containment::consent::FdaProbe {
            state: aterm_containment::consent::FdaState::Granted,
            label: aterm_containment::consent::ProbeLabel::OpenOk,
        },
    );
    app.tick_macos_access_card(t0 + Duration::from_secs(30));
    assert_eq!(app.consent_card.phase(), CardPhase::Settled);
    assert!(fda_entries(&app).is_empty());
    assert_eq!(
        app.messages.wanted_rows(),
        0,
        "nothing wants the glass, before or after"
    );
    assert!(matches!(
        app.messages.log().get(id).and_then(LogRecord::retired),
        Some(Retired::Resolved(Outcome::Ok))
    ));
    let granted = app
        .messages
        .log()
        .records()
        .next_back()
        .expect("the grant is on record");
    assert_eq!(granted.title, GRANTED_CAPTION);
    assert_eq!(granted.key.as_deref(), Some(KEY_FILE_ACCESS));
    assert_eq!(app.next_macos_access_card_deadline(t0), None);

    // A REGRESSION is news, and a grant ends that entry too.
    app.raise_fda_need(
        &super::consent_card::NeedWords {
            cause: super::consent_card::NeedCause::ProtectedRead { session: None },
            tab_number: None,
            regressed: true,
        },
        t0,
    );
    assert_eq!(fda_entries(&app)[0].key, "privacy.fda.regressed");
    app.resolve_fda_need(true, t0);
    assert!(fda_entries(&app).is_empty());
}

/// Disabling the notice from any tab retracts the access question — the
/// entry of the digest (or a row an older parent carried, or its route
/// words), `resolve_key_prefix("privacy.")` — and settles the card;
/// OTHER messages keep their rows (design §7.2, the retired :34162 in
/// spirit).
///
/// macOS ONLY — and NOT so this box stops seeing a red.
/// `tick_macos_access_card` returns on its first statement off macOS, so
/// every assertion below would be measuring that early return instead of
/// the retraction, and the retraction — the thing the test is named for
/// — would be proved on no platform at all. What the early return owes
/// every other host is asserted next door, in
/// `macos_access_card_stays_inert_off_macos`; neither platform is left
/// with an empty claim.
#[test]
#[cfg(target_os = "macos")]
fn macos_access_card_honors_live_policy_without_dismissing_other_messages() {
    use super::consent_card::{CardPhase, Verdict};
    use super::message_reporters::KEY_FILE_ACCESS;
    use aterm_messages::{Message, Severity, tags};
    use std::time::Instant;

    let now = Instant::now();
    for master_off in [false, true] {
        for raised in [false, true] {
            for other in [false, true] {
                let mut app = access_card_windowed();
                assert!(app.consent_card.begin_deciding(now));
                assert!(app.consent_card.on_decided(&Verdict::Offer, now));
                if raised {
                    app.tick_macos_access_card(now);
                    assert_eq!(fda_entries(&app).len(), 1);
                }
                let other_id = other.then(|| {
                    app.post_message(
                        Message::new(tags::PACKAGES, Severity::Warn, "someone else's row")
                            .key("packages.other"),
                    )
                });
                app.config.privacy = Some(crate::app_config::PrivacyConfig {
                    enabled: Some(!master_off),
                    notice: Some(master_off),
                    ..Default::default()
                });
                app.tick_macos_access_card(now);
                assert_eq!(app.consent_card.phase(), CardPhase::Settled);
                assert!(
                    fda_entries(&app).is_empty()
                        && app.messages.live_by_key(KEY_FILE_ACCESS).is_none(),
                    "the policy retracted the access question"
                );
                assert_eq!(app.consent_card_message, None);
                if let Some(id) = other_id {
                    assert!(
                        app.messages.live(id).is_some(),
                        "another reporter's row is left alone"
                    );
                }
                // A delayed worker cannot resurrect an opted-out card.
                app.decide_macos_access_card(crate::consent_card::PendingDecision::launch(None));
                assert_eq!(app.consent_card.phase(), CardPhase::Settled);
                assert!(app.messages.live_by_key(KEY_FILE_ACCESS).is_none());
            }
        }
    }
}

/// Off macOS the tick returns before reading switches, spawning or touching the band: a
/// Linux build never posts a macOS-permission row or resolves another reporter's row.
/// The third row forces the card past any reachable phase with a planted question row;
/// both must be left exactly as found.
#[test]
#[cfg(not(target_os = "macos"))]
fn macos_access_card_stays_inert_off_macos() {
    use super::consent_card::{CardPhase, Verdict};
    use super::message_reporters::{KEY_FILE_ACCESS, file_access_question};
    use aterm_messages::{Message, Severity, tags};
    use std::time::Instant;

    let now = Instant::now();
    fn app_with(enabled: bool, notice: bool) -> App {
        let mut app = App::headless_for_test();
        app.headless = false; // probes and gestures stay inert
        app.config.privacy = Some(crate::app_config::PrivacyConfig {
            enabled: Some(enabled),
            notice: Some(notice),
            ..Default::default()
        });
        app
    }

    for enabled in [false, true] {
        for notice in [false, true] {
            let switches = format!("enabled={enabled} notice={notice}");

            // The launch one-shot's own starting phase — on macOS this is
            // the tick that spawns the marker-reading worker.
            let mut app = app_with(enabled, notice);
            app.tick_macos_access_card(now);
            assert_eq!(
                app.consent_card.phase(),
                CardPhase::Idle,
                "no verdict is ever asked for off macOS ({switches})"
            );
            assert!(
                app.messages.live_by_key(KEY_FILE_ACCESS).is_none(),
                "an inert card never posts ({switches})"
            );

            // Another reporter's row: untouched.
            let mut app = app_with(enabled, notice);
            let other = app.post_message(Message::new(tags::PACKAGES, Severity::Warn, "x"));
            app.tick_macos_access_card(now);
            assert_eq!(app.consent_card.phase(), CardPhase::Idle, "({switches})");
            assert!(app.messages.live(other).is_some(), "({switches})");

            // The MUTANT. Even an opted-out card whose row is already on
            // the band stays exactly where it stands: retracting it is the
            // macOS arm's claim, and this platform never gets that far.
            let mut app = app_with(enabled, notice);
            assert!(app.consent_card.begin_deciding(now));
            assert!(app.consent_card.on_decided(&Verdict::Offer, now));
            app.consent_card.on_raised(now);
            let id = app.post_message(file_access_question(
                &super::consent_card::NeedWords::sample(),
            ));
            app.consent_card_message = Some(id);
            app.tick_macos_access_card(now);
            assert_eq!(
                app.consent_card.phase(),
                CardPhase::Watching { since: now },
                "the guard runs before any policy read ({switches})"
            );
            assert!(
                app.messages.live(id).is_some(),
                "…and before any resolve ({switches})"
            );
        }
    }
}

/// An observed dialog's banner names the tab that asked, as every herald
/// banner does: `<tab title>: <what>`. NEGATIVE CONTROL: a session the
/// store does not hold keeps the bare body.
#[test]
fn an_observed_dialog_banner_names_the_asking_tab() {
    let app = App::headless_for_test();
    app.store
        .read()
        .unwrap_or_else(|p| p.into_inner())
        .by_local(0)
        .expect("the headless instance has tab 0")
        .ctx
        .meta
        .lock()
        .unwrap_or_else(|p| p.into_inner())
        .user_title = Some("build agent".to_string());
    let prompt = |session| super::consent_observer::ObservedPrompt {
        session,
        blocked_on: String::new(),
        service: "kTCCServiceSystemPolicyDocumentsFolder".to_string(),
    };
    let bare = prompt(0).notice().body;
    assert_eq!(
        app.consent_prompt_notice(&prompt(0)).body,
        format!("build agent: {bare}")
    );
    assert_eq!(app.consent_prompt_notice(&prompt(777)).body, bare);
}

/// The PER-SESSION producer (the Claude Code footer's repository read,
/// `Wake::ProtectedRead`): a refused read under a protected root raises the
/// attention for exactly that session's tab; the same refusal outside every
/// protected root is an ordinary permission error and raises nothing.
#[test]
fn a_refused_read_under_a_protected_root_marks_that_sessions_tab() {
    let session = |app: &App| {
        app.front_terminal(WindowId(0))
            .expect("headless_for_test seeds one terminal")
            .session
    };
    let marked = |app: &App| {
        app.windows[&WindowId(0)]
            .tab_set
            .tabs()
            .iter()
            .any(|tab| tab.presentation.indicators.attention)
    };
    let roots = |app: &App| app.consent.policy().protected_roots().to_vec();

    // Outside every protected root: nothing.
    let mut app = App::headless_for_test();
    let sid = session(&app);
    let outside = std::path::PathBuf::from("/nonexistent/aterm-not-protected/repo");
    assert!(
        !aterm_containment::consent::is_under_protected_root(&outside, &roots(&app)),
        "precondition: the control path is not protected"
    );
    app.note_protected_read(sid, &outside);
    assert!(
        !marked(&app),
        "an ordinary refusal raises no consent attention"
    );
    assert!(!app.consent_attention.is_spent());

    // Under a protected root: the gate spends its notice either way, but
    // the TAB MARK is the FULL observer's (`observer = true`); the default
    // sentinel leaves the tab alone — the access card's one question
    // carries the action.
    let Some(root) = roots(&app).first().cloned() else {
        aterm_spec::host::could_not_run("no $HOME resolves here, so nothing is protected");
    };
    app.note_protected_read(sid, &root.join("project"));
    assert!(
        !marked(&app),
        "outside Full mode the refused session's tab carries no mark"
    );
    assert!(
        app.consent_attention.is_spent(),
        "the gate spent its notice"
    );
    let mut full = App::headless_for_test();
    full.config.privacy = Some(crate::app_config::PrivacyConfig {
        observer: Some(true),
        ..Default::default()
    });
    let sid = session(&full);
    full.note_protected_read(sid, &root.join("project"));
    assert!(
        marked(&full),
        "Full mode: the session's tab carries the mark"
    );
    // The master switch turns Full off, the EPERM path's mark included.
    let mut stopped = App::headless_for_test();
    stopped.config.privacy = Some(crate::app_config::PrivacyConfig {
        observer: Some(true),
        enabled: Some(false),
        ..Default::default()
    });
    let sid = session(&stopped);
    stopped.note_protected_read(sid, &root.join("project"));
    assert!(
        !marked(&stopped),
        "enabled = false: no Full mode, so no mark"
    );
}

/// THE SENTINEL IS THE DEFAULT, AND IT IS QUIET (plan-tcc §3.4). Which
/// observer an instance actually runs is the config's tri-state with the
/// master switch and two facts applied: `enabled = false` turns every mode
/// off; the sentinel exists only to feed the access card, so it needs both
/// of the card's switches and a windowed macOS instance; Full is an explicit
/// opt-in that runs wherever it is asked for.
#[test]
fn the_effective_observer_mode_follows_the_config_and_the_cards_switches() {
    use super::consent_observer::ObserverMode;
    let privacy = |observer: Option<bool>, enabled: bool, notice: bool| {
        let mut app = App::headless_for_test();
        app.headless = false;
        app.config.privacy = Some(crate::app_config::PrivacyConfig {
            observer,
            enabled: Some(enabled),
            notice: Some(notice),
            ..Default::default()
        });
        app
    };
    let sentinel_here = if cfg!(target_os = "macos") {
        ObserverMode::Sentinel
    } else {
        ObserverMode::Off
    };
    // (observer, enabled, notice, bundled) -> mode. `enabled = false` is the
    // master switch and turns every mode off; the sentinel needs both of the
    // card's switches and a bundle id to judge a dialog against; Full is an
    // explicit opt-in that needs neither.
    for (observer, enabled, notice, bundled, mode) in [
        (None, true, true, true, sentinel_here),
        (None, true, false, true, ObserverMode::Off),
        (None, false, true, true, ObserverMode::Off),
        (Some(false), true, true, true, ObserverMode::Off),
        (Some(true), true, false, true, ObserverMode::Full),
        (Some(true), false, true, true, ObserverMode::Off),
        (None, true, true, false, ObserverMode::Off),
        (Some(true), true, true, false, ObserverMode::Full),
    ] {
        assert_eq!(
            privacy(observer, enabled, notice).effective_observer_mode_for(bundled),
            mode,
            "observer={observer:?} enabled={enabled} notice={notice} bundled={bundled}"
        );
    }
    // A headless instance never runs the sentinel (inert arm, no card).
    let mut headless = App::headless_for_test();
    assert_eq!(headless.effective_observer_mode(), ObserverMode::Off);
    assert_eq!(
        headless.reconcile_consent_observer(),
        None,
        "a fresh observer already has what Off asks for"
    );
    headless.config.privacy = Some(crate::app_config::PrivacyConfig {
        observer: Some(true),
        ..Default::default()
    });
    assert_eq!(headless.effective_observer_mode(), ObserverMode::Full);
    let mut stopped = App::headless_for_test();
    stopped.config.privacy = Some(crate::app_config::PrivacyConfig {
        observer: Some(true),
        enabled: Some(false),
        ..Default::default()
    });
    assert_eq!(stopped.effective_observer_mode(), ObserverMode::Off);
    assert_eq!(
        headless.reconcile_consent_observer(),
        Some(super::consent_observer::StartOutcome::Started),
        "Full runs where it is asked for: the inert arm here"
    );
}

/// What the test observer arms report. A `fn` arm can speak only through a
/// static, so the tests that install one hold [`OBSERVER_ARM_LOCK`].
struct ObserverArmLog {
    /// The mode of every worker an arm was started for, in order.
    starts: Vec<super::consent_observer::ObserverMode>,
    /// How many `Started` messages [`streaming_observer_arm`] has posted.
    posted: usize,
    /// Set by the test: every standing worker may end.
    release: bool,
    /// Per ended worker: whether the host had dropped its receiver.
    receiver_gone: Vec<bool>,
}

static OBSERVER_ARM: (std::sync::Mutex<ObserverArmLog>, std::sync::Condvar) = (
    std::sync::Mutex::new(ObserverArmLog {
        starts: Vec::new(),
        posted: 0,
        release: false,
        receiver_gone: Vec::new(),
    }),
    std::sync::Condvar::new(),
);

static OBSERVER_ARM_LOCK: std::sync::Mutex<()> = std::sync::Mutex::new(());

/// The arm log, emptied for a new test. Call with [`OBSERVER_ARM_LOCK`] held.
fn reset_observer_arm() {
    let mut log = OBSERVER_ARM.0.lock().unwrap_or_else(|p| p.into_inner());
    log.starts.clear();
    log.posted = 0;
    log.release = false;
    log.receiver_gone.clear();
}

/// Every standing test worker may end. Call with [`OBSERVER_ARM_LOCK`] held.
fn release_observer_arm() {
    let (log, changed) = &OBSERVER_ARM;
    log.lock().unwrap_or_else(|p| p.into_inner()).release = true;
    changed.notify_all();
}

/// Wait until `ready` holds over the arm log: a one-minute hang detector.
fn await_observer_arm(what: &str, ready: impl Fn(&ObserverArmLog) -> bool) {
    let (log, changed) = &OBSERVER_ARM;
    let log = log.lock().unwrap_or_else(|p| p.into_inner());
    let (_log, waited) = changed
        .wait_timeout_while(log, std::time::Duration::from_secs(60), |log| !ready(log))
        .unwrap_or_else(|p| p.into_inner());
    assert!(
        !waited.timed_out(),
        "the observer arm never reached: {what}"
    );
}

/// A stream that stands like a live one: it records its mode, posts
/// `Started`, and holds its sender until the test releases it, then reports
/// whether the host still held the receiver.
fn streaming_observer_arm(
    sink: &super::consent_observer::StreamSink,
    mode: super::consent_observer::ObserverMode,
) {
    let (log, changed) = &OBSERVER_ARM;
    log.lock()
        .unwrap_or_else(|p| p.into_inner())
        .starts
        .push(mode);
    sink.post_for_test(super::consent_observer::ObserverEvent::Started);
    let mut held = log.lock().unwrap_or_else(|p| p.into_inner());
    held.posted += 1;
    changed.notify_all();
    // Bounded, so a test that failed before its release leaks no thread.
    let (held, _) = changed
        .wait_timeout_while(held, std::time::Duration::from_secs(60), |log| !log.release)
        .unwrap_or_else(|p| p.into_inner());
    drop(held);
    let kept = sink.post_for_test(super::consent_observer::ObserverEvent::Started);
    log.lock()
        .unwrap_or_else(|p| p.into_inner())
        .receiver_gone
        .push(!kept);
    changed.notify_all();
}

/// A stream that cannot read the log: it records its mode, says so and ends.
fn log_refused_observer_arm(
    sink: &super::consent_observer::StreamSink,
    mode: super::consent_observer::ObserverMode,
) {
    OBSERVER_ARM
        .0
        .lock()
        .unwrap_or_else(|p| p.into_inner())
        .starts
        .push(mode);
    sink.post_for_test(super::consent_observer::ObserverEvent::Unavailable(
        super::consent_observer::UnavailableReason::NoLogAccess,
    ));
}

fn observer_arm_starts() -> Vec<super::consent_observer::ObserverMode> {
    OBSERVER_ARM
        .0
        .lock()
        .unwrap_or_else(|p| p.into_inner())
        .starts
        .clone()
}

/// RECONCILED AT THE EVENTS THAT CHANGE IT. An admitted config generation
/// starts the stream it asks for and stops the one it no longer wants, so
/// `observer log=` reads `off` after an owner turns the observer off, both
/// from a live stream and from one that already ended. A generation that
/// leaves the wish alone starts and stops nothing (the negative control),
/// and that includes a stream that ended by itself: it is not respawned.
/// A cold `exec` that returned restarts what its preparation stopped.
///
/// A worker records its start on its own thread, so a start is awaited on
/// the arm log; "nothing changed" is read synchronously off the observer,
/// since a stop clears the running mode and a start takes a receiver at once.
#[test]
fn a_reload_reconciles_the_observer() {
    use super::consent_observer::ObserverMode;
    use crate::app_colorscheme::install_resolved_colors::apply_config_text;
    let _arm = OBSERVER_ARM_LOCK.lock().unwrap_or_else(|p| p.into_inner());
    reset_observer_arm();
    let dir = std::env::temp_dir().join(format!("aterm-observer-reconcile-{}", std::process::id()));
    std::fs::create_dir_all(&dir).expect("scratch dir");
    let full = "[privacy]\nobserver = true\n";
    let off = "[privacy]\nobserver = false\n";
    let unrelated = "[privacy]\nobserver = true\n[claude_footer]\nusage = false\n";

    let mut app = App::headless_for_test();
    app.consent_observer
        .set_arm_for_test(streaming_observer_arm);
    apply_config_text(&mut app, &dir, full);
    assert!(
        app.consent_observer.is_live(),
        "the reload started a stream"
    );
    await_observer_arm("the first Started", |log| log.posted == 1);
    assert_eq!(observer_arm_starts(), [ObserverMode::Full]);
    app.poll_consent_observer(std::time::Instant::now());
    assert_eq!(app.consent_observer.running(), ObserverMode::Full);
    assert_eq!(app.privacy_observer_log(), "ok");

    apply_config_text(&mut app, &dir, unrelated);
    assert_eq!(
        app.config.claude_footer.as_ref().and_then(|f| f.usage),
        Some(false),
        "the unrelated generation was admitted"
    );
    assert_eq!(
        app.consent_observer.running(),
        ObserverMode::Full,
        "an unchanged wish stops nothing, so it starts nothing"
    );
    assert!(app.consent_observer.is_live());

    #[cfg(unix)]
    {
        app.prepare_cold_exec(7, true, false);
        assert_eq!(app.consent_observer.running(), ObserverMode::Off);
        assert!(!app.consent_observer.is_live());
        app.resume_after_cold_exec();
        assert!(
            app.consent_observer.is_live(),
            "a returned exec restarts the stream"
        );
        await_observer_arm("the second Started", |log| log.posted == 2);
        assert_eq!(
            observer_arm_starts(),
            [ObserverMode::Full, ObserverMode::Full]
        );
        app.poll_consent_observer(std::time::Instant::now());
        assert_eq!(app.consent_observer.running(), ObserverMode::Full);
    }
    let workers = if cfg!(unix) { 2 } else { 1 };

    apply_config_text(&mut app, &dir, off);
    assert_eq!(app.consent_observer.running(), ObserverMode::Off);
    assert!(!app.consent_observer.is_live());
    assert_eq!(app.privacy_observer_log(), "off");
    // The master switch outranks the opt-in: nothing starts.
    apply_config_text(
        &mut app,
        &dir,
        "[privacy]\nobserver = true\nenabled = false\n",
    );
    assert!(
        !app.consent_observer.is_live(),
        "enabled = false starts nothing"
    );
    assert_eq!(app.consent_observer.requested(), ObserverMode::Off);
    assert_eq!(app.privacy_observer_log(), "off");
    release_observer_arm();
    await_observer_arm("every worker ended", |log| {
        log.receiver_gone.len() == workers
    });
    let log = OBSERVER_ARM.0.lock().unwrap_or_else(|p| p.into_inner());
    assert_eq!(log.starts.len(), workers, "{:?}", log.starts);
    assert!(
        log.receiver_gone.iter().all(|gone| *gone),
        "every stopped worker's sender is disconnected"
    );
    drop(log);

    // A stream that ended by itself (no log access) keeps its request: an
    // unrelated generation does not respawn it, and turning the observer off
    // reads `off`, not the stale `unavailable`.
    reset_observer_arm();
    let mut ended = App::headless_for_test();
    ended
        .consent_observer
        .set_arm_for_test(log_refused_observer_arm);
    apply_config_text(&mut ended, &dir, full);
    await_observer_arm("the refused start", |log| log.starts.len() == 1);
    // A hang detector for the worker's end, which only a drain can see.
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(60);
    while ended.consent_observer.is_live() && std::time::Instant::now() < deadline {
        ended.poll_consent_observer(std::time::Instant::now());
        std::thread::yield_now();
    }
    assert!(
        !ended.consent_observer.is_live(),
        "the refused worker ended"
    );
    assert_eq!(ended.privacy_observer_log(), "unavailable");
    apply_config_text(&mut ended, &dir, unrelated);
    assert!(
        !ended.consent_observer.is_live(),
        "an ended stream is not respawned by an unchanged wish"
    );
    assert_eq!(ended.privacy_observer_log(), "unavailable");
    apply_config_text(&mut ended, &dir, off);
    assert_eq!(ended.privacy_observer_log(), "off");
    assert_eq!(observer_arm_starts(), [ObserverMode::Full]);
    let _ = std::fs::remove_dir_all(&dir);
}

/// The pid the observer tests give every session's shell, so a request from
/// it is attributable without asking the OS for a process group.
const OBSERVED_SHELL: i32 = 4711;

/// A pid no process has (above every platform's pid ceiling), so `getpgid`
/// answers nothing and a request from it is nobody's.
const UNOWNED_PID: i32 = 0x7FFF_FFF0;

/// A headless instance whose sessions' shell is [`OBSERVED_SHELL`], with the
/// standing test arm installed.
fn app_with_observed_shell() -> App {
    let mut app = App::headless_for_test();
    set_shell_pid(&mut app, OBSERVED_SHELL);
    app.consent_observer
        .set_arm_for_test(streaming_observer_arm);
    app
}

/// Give every pooled session's shell `pid`, in place: no insert, so the
/// roster generation does not move.
fn set_shell_pid(app: &mut App, pid: i32) {
    for pooled in app.pool.sessions.values_mut() {
        pooled.session.pid = pid;
    }
}

/// Whether a tab of the headless window carries the attention mark.
fn a_tab_is_marked(app: &App) -> bool {
    app.windows[&WindowId(0)]
        .tab_set
        .tabs()
        .iter()
        .any(|tab| tab.presentation.indicators.attention)
}

/// Feed `app`'s observer a Documents request from [`OBSERVED_SHELL`] that
/// opened ten seconds ago and has no result: past the prompt threshold, so a
/// running Full stream announces it at its next poll.
fn feed_open_request(app: &mut App, msg_id: &str) {
    let opened = std::time::Instant::now()
        .checked_sub(std::time::Duration::from_secs(10))
        .expect("the monotonic clock reads past ten seconds");
    feed_request_opened_at(app, msg_id, OBSERVED_SHELL, opened);
}

/// Feed `app`'s observer a Documents request from `accessing_pid` that
/// opened at `opened` and has no result.
fn feed_request_opened_at(
    app: &mut App,
    msg_id: &str,
    accessing_pid: i32,
    opened: std::time::Instant,
) {
    use super::consent_observer::{ObserverEvent, TccEvent};
    for line in [
        TccEvent::Ctx {
            msg_id: msg_id.to_owned(),
            service: "kTCCServiceSystemPolicyDocumentsFolder".to_owned(),
            preflight: Some(false),
        },
        TccEvent::Attribution {
            msg_id: msg_id.to_owned(),
            accessing_pid: Some(accessing_pid),
            requesting_pid: None,
        },
    ] {
        app.consent_observer
            .feed_for_test(ObserverEvent::Line(line), opened);
    }
}

/// The park's announcement follows the stream that is RUNNING, not the
/// config: a Full stream announces a prompt it observed even when the config
/// was edited behind the reconcile's back, after a reload to Off nothing is
/// announced, and a running sentinel announces nothing either.
///
/// No case here tells `running` from `requested`: with Full requested, no
/// state a real worker can reach has them differ with anything to announce.
/// Before the stream's `Started` its table is empty (emptied at the spawn,
/// and lines follow `Started`), and after its end the availability is not
/// `ok`, so `take_new_verdicts` returns nothing.
#[test]
fn the_park_reads_the_running_mode_not_the_config() {
    use super::consent_observer::ObserverMode;
    use crate::app_colorscheme::install_resolved_colors::apply_config_text;
    use std::time::Instant;
    let _arm = OBSERVER_ARM_LOCK.lock().unwrap_or_else(|p| p.into_inner());
    reset_observer_arm();
    let dir = std::env::temp_dir().join(format!("aterm-observer-park-{}", std::process::id()));
    std::fs::create_dir_all(&dir).expect("scratch dir");

    let mut app = app_with_observed_shell();
    apply_config_text(&mut app, &dir, "[privacy]\nobserver = true\n");
    await_observer_arm("Started", |log| log.posted == 1);
    app.poll_consent_observer(Instant::now());
    assert_eq!(app.consent_observer.running(), ObserverMode::Full);

    // Behind the reconcile's back: the park must not re-derive the mode.
    let reloaded = app.config.privacy.take();
    app.config.privacy = Some(crate::app_config::PrivacyConfig {
        observer: Some(false),
        ..Default::default()
    });
    feed_open_request(&mut app, "7.1");
    assert!(!a_tab_is_marked(&app), "precondition");
    app.poll_consent_observer(Instant::now());
    assert!(
        a_tab_is_marked(&app),
        "the running Full stream announces its prompt"
    );
    // Undone, so the reload below is a change the admission path applies.
    app.config.privacy = reloaded;

    app.windows
        .get_mut(&WindowId(0))
        .and_then(|window| window.tab_set.tab_at_mut(0))
        .expect("the headless instance has tab 0")
        .presentation
        .indicators
        .attention = false;
    assert!(!a_tab_is_marked(&app), "precondition");
    apply_config_text(&mut app, &dir, "[privacy]\nobserver = false\n");
    assert_eq!(app.consent_observer.running(), ObserverMode::Off);
    feed_open_request(&mut app, "7.2");
    app.poll_consent_observer(Instant::now());
    assert!(
        !a_tab_is_marked(&app),
        "after a reload to Off nothing is announced"
    );

    // A running sentinel with an attributable open request in its table: the
    // availability is `ok`, so only the mode check keeps the tab unmarked.
    assert_eq!(
        app.consent_observer.start(ObserverMode::Sentinel, || {}),
        super::consent_observer::StartOutcome::Started
    );
    await_observer_arm("the sentinel's Started", |log| log.posted == 2);
    app.poll_consent_observer(Instant::now());
    assert_eq!(app.consent_observer.running(), ObserverMode::Sentinel);
    assert_eq!(app.privacy_observer_log(), "ok");
    feed_open_request(&mut app, "7.3");
    app.poll_consent_observer(Instant::now());
    assert!(
        !a_tab_is_marked(&app),
        "a running sentinel announces nothing"
    );
    app.consent_observer.stop();

    release_observer_arm();
    await_observer_arm("every worker ended", |log| log.receiver_gone.len() == 2);
    let _ = std::fs::remove_dir_all(&dir);
}

/// THE THRESHOLD CROSSING WAKES THE PARK. A request a running Full stream
/// observed is announced when it reaches the prompt threshold with nothing
/// else moving: the poll that folded it arms the crossing as the consent
/// lanes' park deadline, under its own owner; a poll before it judges
/// nothing and the poll at it announces. A passed crossing is not armed
/// again, and a stopped stream leaves nothing armed.
///
/// An older request from a pid nobody owns stands past the threshold. For
/// the poll before the crossing, the shell takes that pid in place, which
/// moves no roster generation: a judging pass there would find it an owner
/// and mark the tab, so an unmarked tab says the poll judged nothing.
#[test]
fn the_threshold_crossing_is_a_park_deadline() {
    use super::consent_observer::{ObserverMode, PENDING_PROMPT_THRESHOLD};
    use crate::app_colorscheme::install_resolved_colors::apply_config_text;
    use crate::metrics::DeadlineOwner;
    use std::time::{Duration, Instant};
    let fold = |app: &App, now: Instant| {
        let (mut deadline, mut owner) = (None, DeadlineOwner::None);
        app.fold_consent_lane_deadlines(now, &mut deadline, &mut owner);
        (deadline, owner)
    };
    let _arm = OBSERVER_ARM_LOCK.lock().unwrap_or_else(|p| p.into_inner());
    reset_observer_arm();
    let dir = std::env::temp_dir().join(format!("aterm-observer-crossing-{}", std::process::id()));
    std::fs::create_dir_all(&dir).expect("scratch dir");

    let mut app = app_with_observed_shell();
    apply_config_text(&mut app, &dir, "[privacy]\nobserver = true\n");
    await_observer_arm("Started", |log| log.posted == 1);
    let opened = Instant::now();
    app.poll_consent_observer(opened);
    assert_eq!(app.consent_observer.running(), ObserverMode::Full);
    assert_eq!(fold(&app, opened).0, None, "nothing open arms nothing");

    let opened_before = opened
        .checked_sub(PENDING_PROMPT_THRESHOLD + Duration::from_secs(1))
        .expect("the monotonic clock reads past the threshold");
    feed_request_opened_at(&mut app, "9.0", UNOWNED_PID, opened_before);
    feed_request_opened_at(&mut app, "9.1", OBSERVED_SHELL, opened);
    app.poll_consent_observer(opened);
    assert!(
        !a_tab_is_marked(&app),
        "a request nobody owns marks nothing"
    );
    let crossing = opened + PENDING_PROMPT_THRESHOLD;
    assert_eq!(
        fold(&app, opened),
        (Some(crossing), DeadlineOwner::ConsentObserver)
    );
    set_shell_pid(&mut app, UNOWNED_PID);
    app.poll_consent_observer(crossing - Duration::from_millis(1));
    assert!(!a_tab_is_marked(&app), "nothing is judged before it");
    set_shell_pid(&mut app, OBSERVED_SHELL);
    app.poll_consent_observer(crossing);
    assert!(a_tab_is_marked(&app), "the crossing announces on its own");
    assert_eq!(
        fold(&app, crossing).0,
        None,
        "a passed crossing is not armed"
    );

    feed_request_opened_at(&mut app, "9.2", OBSERVED_SHELL, crossing);
    app.poll_consent_observer(crossing);
    assert!(fold(&app, crossing).0.is_some(), "precondition");
    apply_config_text(&mut app, &dir, "[privacy]\nobserver = false\n");
    assert_eq!(
        fold(&app, crossing).0,
        None,
        "a stopped stream arms nothing"
    );

    release_observer_arm();
    await_observer_arm("the worker ended", |log| log.receiver_gone.len() == 1);
    let _ = std::fs::remove_dir_all(&dir);
}

/// A RESTARTED STREAM STARTS WITH NOTHING IN FLIGHT. A request the stopped
/// stream was correlating can no longer see its result: its dialog may have
/// been answered while the observer was off. So after a reload Full -> Off
/// -> Full the poll announces none of it, though it is attributable and
/// past its threshold; a request the new stream observes is announced (the
/// negative control).
#[test]
fn a_restarted_stream_announces_nothing_the_stopped_one_was_correlating() {
    use super::consent_observer::ObserverMode;
    use crate::app_colorscheme::install_resolved_colors::apply_config_text;
    use std::time::Instant;
    let _arm = OBSERVER_ARM_LOCK.lock().unwrap_or_else(|p| p.into_inner());
    reset_observer_arm();
    let dir = std::env::temp_dir().join(format!("aterm-observer-restart-{}", std::process::id()));
    std::fs::create_dir_all(&dir).expect("scratch dir");
    let full = "[privacy]\nobserver = true\n";

    let mut app = app_with_observed_shell();
    apply_config_text(&mut app, &dir, full);
    await_observer_arm("the first Started", |log| log.posted == 1);
    app.poll_consent_observer(Instant::now());
    assert_eq!(app.consent_observer.running(), ObserverMode::Full);
    // Observed by the first stream, which stops before a park announces it.
    feed_open_request(&mut app, "8.1");
    apply_config_text(&mut app, &dir, "[privacy]\nobserver = false\n");
    assert_eq!(app.consent_observer.running(), ObserverMode::Off);

    apply_config_text(&mut app, &dir, full);
    await_observer_arm("the restart's Started", |log| log.posted == 2);
    app.poll_consent_observer(Instant::now());
    assert_eq!(app.consent_observer.running(), ObserverMode::Full);
    assert!(
        !a_tab_is_marked(&app),
        "the stopped stream's request is not announced"
    );
    feed_open_request(&mut app, "8.2");
    app.poll_consent_observer(Instant::now());
    assert!(
        a_tab_is_marked(&app),
        "the restarted stream announces what it observes"
    );

    apply_config_text(&mut app, &dir, "[privacy]\nobserver = false\n");
    release_observer_arm();
    await_observer_arm("every worker ended", |log| log.receiver_gone.len() == 2);
    let log = OBSERVER_ARM.0.lock().unwrap_or_else(|p| p.into_inner());
    assert!(
        log.receiver_gone.iter().all(|gone| *gone),
        "every stopped worker's sender is disconnected"
    );
    drop(log);
    let _ = std::fs::remove_dir_all(&dir);
}

/// THE LINK FROM THE SENTINEL TO THE CARD. A dialog the observer saw for
/// this app's bundle id, for the app-data service, becomes one need of the
/// access card; a dialog for another service (its Allow is durable) or
/// another subject becomes none. Every step of the mapping is on the path
/// this drives: `feed` (the worker's message), `poll_consent_observer` (the
/// drain, which takes nothing until the bundle identity is warm) and
/// `note_observed_prompts` (service mapping, tab resolution,
/// `note_fda_need`).
#[test]
#[cfg(target_os = "macos")]
fn an_observed_app_data_dialog_for_this_bundle_is_a_need_and_nothing_else_is() {
    use super::consent_observer::{ObserverEvent, PromptNeed};
    use std::time::{Duration, Instant};
    let bundle = "com.aterm.aterm";
    let prompt = |msg_id: &str, service: &str, subject: &str, accessing: Option<i32>| {
        ObserverEvent::Prompt(PromptNeed {
            msg_id: msg_id.to_owned(),
            service: service.to_owned(),
            subject: subject.to_owned(),
            resp_pid: 4242,
            accessing_pid: accessing,
        })
    };
    let app_data = "kTCCServiceSystemPolicyAppData";
    let mut app = App::headless_for_test();
    app.headless = false;
    let now = Instant::now();

    // (a) The app-data dialog for the running bundle id, with the accessing
    // pid UNRESOLVED (no shell of this instance is its ancestor): (c) it
    // still counts, with no tab in the words.
    app.consent_observer
        .feed_for_test(prompt("1.1", app_data, bundle, Some(i32::MAX - 1)), now);
    // The identity is not warm in a test binary, so the poll takes nothing
    // and drops nothing: the dialog waits for the moment it can be judged.
    // A requested stream is what makes the park poll at all.
    assert_eq!(
        app.consent_observer
            .start(super::consent_observer::ObserverMode::Sentinel, || {}),
        super::consent_observer::StartOutcome::Started
    );
    app.poll_consent_observer(now);
    assert_eq!(app.consent_card_host.needs, 0, "nothing judged while cold");
    app.note_observed_prompts(Some(bundle), now);
    assert_eq!(
        app.consent_card_host.needs, 1,
        "the app-data dialog is a need"
    );

    // (b) A Documents dialog, and an app-data dialog for another subject,
    // are not this card's need.
    app.consent_observer.feed_for_test(
        prompt(
            "1.2",
            "kTCCServiceSystemPolicyDocumentsFolder",
            bundle,
            None,
        ),
        now,
    );
    app.consent_observer
        .feed_for_test(prompt("1.3", app_data, "com.example.other", None), now);
    app.note_observed_prompts(Some(bundle), now + Duration::from_secs(120));
    assert_eq!(app.consent_card_host.needs, 1, "neither is a need");

    // An unresolved accessing pid (None) still counts.
    app.consent_observer
        .feed_for_test(prompt("1.4", app_data, bundle, None), now);
    app.note_observed_prompts(Some(bundle), now + Duration::from_secs(240));
    assert_eq!(app.consent_card_host.needs, 2);
}

/// A DIALOG THE CARD IS NOT ABOUT, ON RECORD. The 2026-10-16 media-library
/// dialog was seen and dropped to one `aterm.log` line, so nothing aterm shows
/// could say which tab asked. Now each dialog for this bundle in a service
/// aterm can name is one record keyed by service (never a row, never a need);
/// a service aterm cannot name, a dialog for another subject, the app-data
/// dialog and aterm's own read record nothing.
#[test]
#[cfg(target_os = "macos")]
fn an_observed_dialog_the_card_is_not_about_is_one_record_per_service() {
    use super::consent_observer::{ObserverEvent, PromptNeed};
    use std::time::Instant;
    let bundle = "com.aterm.aterm";
    let prompt = |msg_id: &str, service: &str, subject: &str| {
        ObserverEvent::Prompt(PromptNeed {
            msg_id: msg_id.to_owned(),
            service: service.to_owned(),
            subject: subject.to_owned(),
            resp_pid: 4242,
            accessing_pid: Some(i32::MAX - 1),
        })
    };
    let mut app = App::headless_for_test();
    app.headless = false;
    let now = Instant::now();
    let dialogs = |app: &App| -> Vec<(String, String)> {
        app.messages
            .log()
            .records()
            .filter_map(|r| {
                r.key
                    .as_deref()
                    .filter(|k| k.starts_with("privacy.dialog."))
                    .map(|k| (k.to_owned(), r.title.clone()))
            })
            .collect()
    };

    for (id, service, subject) in [
        ("2.1", "kTCCServiceMediaLibrary", bundle),
        ("2.2", "kTCCServiceMediaLibrary", bundle),
        ("2.3", "kTCCServiceAddressBook", bundle),
        ("2.4", "kTCCServiceNoSuchService", bundle),
        ("2.5", "kTCCServicePhotos", "com.example.other"),
        ("2.6", "kTCCServiceSystemPolicyAppData", bundle),
    ] {
        app.consent_observer
            .feed_for_test(prompt(id, service, subject), now);
    }
    // aterm's own read (the warm-up) is on the Security page, not on record.
    app.consent_observer.feed_for_test(
        ObserverEvent::Prompt(PromptNeed {
            msg_id: "2.7".to_owned(),
            service: "kTCCServicePhotos".to_owned(),
            subject: bundle.to_owned(),
            resp_pid: 4242,
            accessing_pid: i32::try_from(std::process::id()).ok(),
        }),
        now,
    );
    app.note_observed_prompts(Some(bundle), now);

    let seen = dialogs(&app);
    assert_eq!(
        seen.iter()
            .filter(|(k, _)| k == "privacy.dialog.medialibrary")
            .count(),
        2,
        "each dialog is its own record: {seen:?}"
    );
    let keys: std::collections::BTreeSet<&str> = seen.iter().map(|(k, _)| k.as_str()).collect();
    assert_eq!(
        keys,
        ["privacy.dialog.addressbook", "privacy.dialog.medialibrary"]
            .into_iter()
            .collect(),
        "one key per nameable service of this bundle: {seen:?}"
    );
    assert!(
        seen.iter()
            .any(|(_, title)| title == "Asked for the music and media library"),
        "{seen:?}"
    );
    for key in keys {
        assert!(
            app.messages.live_by_key(key).is_none(),
            "{key} is a record, never a row"
        );
    }
    assert_eq!(
        app.consent_card_host.needs, 1,
        "only the app-data dialog is a need"
    );
}

/// AN EPERM UNDER A PROTECTED ROOT IS A NEED AND POSTS NO BANNER OUTSIDE
/// FULL MODE: the once-per-process gate is spent (the status item
/// refreshes) but the card, not a notification, carries it. Headless
/// instances have no card, so the need is not even counted there; a
/// windowed one counts it and hands it to the card.
#[test]
fn an_eperm_under_a_protected_root_is_a_need_and_posts_no_banner_outside_full_mode() {
    use super::consent_card::CardPhase;
    let mut app = App::headless_for_test();
    app.headless = false;
    app.note_protected_eperm(Some(0));
    assert!(
        app.consent_attention.is_spent(),
        "the once-per-process gate is spent"
    );
    if cfg!(target_os = "macos") {
        assert_eq!(app.consent_card_host.needs, 1, "the refusal is a need");
        // No event loop under the harness, so the card keeps the need for
        // the moment it can decide it (never dropped, never posted here).
        assert_eq!(app.consent_card.phase(), CardPhase::Idle);
    } else {
        assert_eq!(app.consent_card_host.needs, 0, "no card off macOS");
    }
    assert!(
        app.messages
            .live_by_key(super::message_reporters::KEY_FILE_ACCESS)
            .is_none(),
        "a need is not a row: decide_need decides that"
    );
}

/// NEEDS ARE COALESCED AND LOGGED SPARINGLY: a burst inside
/// `NEED_DEBOUNCE` is one need for the card (all are counted), the
/// `aterm.log` line is due once per `NEED_LOG_EVERY`, and a need that
/// arrives while an incoming handoff is uncommitted is kept, not lost.
#[test]
#[cfg(target_os = "macos")]
fn note_fda_need_coalesces_and_logs_at_most_every_six_hours() {
    use super::consent_card::{NEED_DEBOUNCE, NEED_LOG_EVERY, NeedCause};
    use std::time::{Duration, Instant};
    let t0 = Instant::now();
    let cause = NeedCause::PromptObserved {
        service: Some(aterm_containment::consent::Folder::AppData),
        session: None,
    };
    let mut app = App::headless_for_test();
    app.headless = false;
    app.note_fda_need(cause, t0);
    assert_eq!(app.consent_card_host.needs, 1);
    assert_eq!(app.consent_card_host.need_logged, Some(t0));
    assert_eq!(app.consent_card_host.need_last, Some(t0));
    // A burst: counted, but neither logged again nor a second need.
    for i in 1..20u64 {
        app.note_fda_need(cause, t0 + Duration::from_millis(i));
    }
    assert_eq!(app.consent_card_host.needs, 20);
    assert_eq!(app.consent_card_host.need_logged, Some(t0));
    assert_eq!(app.consent_card_host.need_last, Some(t0), "coalesced");
    // Past the debounce it is a fresh need, still not a fresh log line.
    app.note_fda_need(cause, t0 + NEED_DEBOUNCE);
    assert_eq!(app.consent_card_host.need_last, Some(t0 + NEED_DEBOUNCE));
    assert_eq!(app.consent_card_host.need_logged, Some(t0));
    // Six hours on, the line is due again.
    let later = t0 + NEED_LOG_EVERY;
    app.note_fda_need(cause, later);
    assert_eq!(app.consent_card_host.need_logged, Some(later));

    // While an incoming handoff is uncommitted the need is KEPT for the
    // moment the card is idle again.
    let mut app = App::headless_for_test();
    app.headless = false;
    app.incoming_handoff_pending = true;
    app.consent_card.settle();
    app.note_fda_need(cause, t0);
    assert_eq!(
        app.consent_card.phase(),
        super::consent_card::CardPhase::Settled,
        "not re-armed while the handoff is uncommitted"
    );
    assert!(app.consent_card.take_pending_need(), "kept, not dropped");
}

/// A DENIED PROBE ALONE RAISES NOTHING: no need, no question — however many
/// parks tick, whatever the cached probe reads. The old card asked at every
/// launch on `denied` alone.
#[test]
#[cfg(target_os = "macos")]
fn a_denied_probe_alone_raises_nothing() {
    use super::consent_card::CardPhase;
    use std::time::{Duration, Instant};
    let mut app = access_card_windowed();
    let t0 = Instant::now();
    for i in 0..5u64 {
        app.tick_macos_access_card(t0 + Duration::from_secs(i));
    }
    assert!(
        app.messages
            .live_by_key(super::message_reporters::KEY_FILE_ACCESS)
            .is_none()
            && fda_entries(&app).is_empty(),
        "a denial with no dialog is not a need"
    );
    // No event loop, so the launch check settles instead of spawning.
    assert_eq!(app.consent_card.phase(), CardPhase::Settled);
    assert_eq!(app.consent_card_host.needs, 0);
}

/// OPEN SETTINGS RECORDS `opened_at` (through either surface): the record
/// is what tells a newer process the owner already acted.
#[test]
#[cfg(target_os = "macos")]
fn open_settings_records_opened_at_in_the_record() {
    use std::time::Instant;
    let mut app = access_card_windowed();
    assert_eq!(app.consent_card_host.record.opened_at, None);
    app.note_macos_access_settings_opened(Instant::now());
    assert!(app.consent_card_host.record.opened_at.is_some());
    assert_eq!(
        app.consent_card_host.marker,
        Some(super::consent_card::Marker::Opened)
    );
    // …and a `not-now` in the cache is never replaced by it.
    app.consent_card_host.marker = Some(super::consent_card::Marker::NotNow);
    app.record_macos_access_marker(super::consent_card::Marker::Opened);
    assert_eq!(
        app.consent_card_host.marker,
        Some(super::consent_card::Marker::NotNow)
    );
}

#[test]
fn overlap_event_classes_exempt_paint_tolerate_typing_and_revoke_structure() {
    use super::{
        UpdateHandoffEventClass as Class, Wake, handoff_deferrable_input,
        input_event_handoff_class, update_handoff_wake_class, update_handoff_window_event_class,
    };
    use aterm_window::dpi::{PhysicalPosition, PhysicalSize};
    use aterm_window::event::{
        DeviceId, ElementState, MouseButton, MouseScrollDelta, TouchPhase, WindowEvent,
    };

    // Paint stays exempt: the frozen frame keeps presenting (seam 5).
    for paint in [
        WindowEvent::RedrawRequested,
        WindowEvent::Occluded(true),
        WindowEvent::Focused(false),
    ] {
        assert_eq!(update_handoff_window_event_class(&paint), Class::Exempt);
    }
    // Pointer and modifier noise buffers through: hover, clicks, scroll,
    // gestures, window drags, a file hovered and taken away, and the theme flip.
    for tolerated in [
        WindowEvent::CursorMoved {
            device_id: DeviceId::dummy(),
            position: PhysicalPosition::new(4.0, 2.0),
        },
        WindowEvent::ModifiersChanged(aterm_window::event::Modifiers::default()),
        WindowEvent::MouseInput {
            device_id: DeviceId::dummy(),
            state: ElementState::Pressed,
            button: MouseButton::Left,
        },
        WindowEvent::MouseWheel {
            device_id: DeviceId::dummy(),
            delta: MouseScrollDelta::LineDelta(0.0, 1.0),
            phase: TouchPhase::Moved,
        },
        WindowEvent::Moved(PhysicalPosition::new(1, 1)),
        WindowEvent::HoveredFileCancelled,
        WindowEvent::ThemeChanged(aterm_window::window::Theme::Dark),
    ] {
        assert_eq!(
            update_handoff_window_event_class(&tolerated),
            Class::Tolerated,
            "{tolerated:?} must buffer through the overlap"
        );
    }
    // Only events that genuinely invalidate the committed identity revoke:
    // grid geometry, destructive intent, a real byte-writing drop, and a
    // mid-composition preedit that cannot survive `_exit`.
    for revoking in [
        WindowEvent::Resized(PhysicalSize::new(800, 600)),
        WindowEvent::CloseRequested,
        WindowEvent::Destroyed,
        WindowEvent::DroppedFile(std::path::PathBuf::from("/tmp/x")),
        WindowEvent::Ime(aterm_window::event::Ime::Preedit("a".to_string(), None)),
    ] {
        assert_eq!(
            update_handoff_window_event_class(&revoking),
            Class::Revoking,
            "{revoking:?} must revoke the overlap"
        );
    }
    // The delivery seam agrees on the grid: a resize revokes there too.
    assert_eq!(
        input_event_handoff_class(&crate::input::InputEvent::Resize {
            rows: 24,
            cols: 80,
            echo_to_window: false,
        }),
        Class::Revoking
    );

    // The incoming side: a visible, uncommitted successor must queue what the user
    // types, compositions included, since an IME commit is a grapheme's only path to
    // the PTY.
    for deferrable in [
        WindowEvent::ModifiersChanged(aterm_window::event::Modifiers::default()),
        WindowEvent::Ime(aterm_window::event::Ime::Preedit("に".to_string(), None)),
        WindowEvent::Ime(aterm_window::event::Ime::Commit("日本語".to_string())),
        WindowEvent::Ime(aterm_window::event::Ime::Enabled),
        WindowEvent::Ime(aterm_window::event::Ime::Disabled),
    ] {
        assert!(
            handoff_deferrable_input(&deferrable),
            "{deferrable:?} must be replayed after Commit, not discarded"
        );
    }
    // Everything else still rides through or revokes; queueing it would replay
    // stale geometry and pointer state into a window that has moved on.
    for passthrough in [
        WindowEvent::RedrawRequested,
        WindowEvent::Resized(PhysicalSize::new(800, 600)),
        WindowEvent::CloseRequested,
        WindowEvent::HoveredFileCancelled,
    ] {
        assert!(
            !handoff_deferrable_input(&passthrough),
            "{passthrough:?} must not be queued as input"
        );
    }

    // Wake side: output/bells/controller-input batches buffer through
    // (per-event policy applies downstream), session exit and structural
    // wakes revoke, and the paint/bookkeeping wakes — including the
    // effect-frame pump — never touch the overlap.
    for tolerated in [
        Wake::Output {
            session: 1,
            window: WindowId(0),
        },
        Wake::Bell {
            session: 1,
            window: WindowId(0),
        },
        Wake::Input {
            mark: None,
            batch: Vec::new(),
            src: crate::input::Source::Human,
            reply: None,
            session: None,
            fence: None,
        },
    ] {
        assert_eq!(update_handoff_wake_class(&tolerated), Class::Tolerated);
    }
    for revoking in [
        Wake::Exit {
            session: 1,
            window: WindowId(0),
        },
        Wake::CreateWindow,
        Wake::DetachActiveTab,
        Wake::ConfirmedClose {
            replay: crate::close_confirm::CloseReplay::Quit,
        },
        Wake::TabSpawned(std::sync::Arc::new(
            crate::tab_spawn::TabSpawnCompletion::default(),
        )),
    ] {
        assert_eq!(update_handoff_wake_class(&revoking), Class::Revoking);
    }
    for exempt in [Wake::Snapshot, Wake::ConfigReload] {
        assert_eq!(update_handoff_wake_class(&exempt), Class::Exempt);
    }
}
