// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use crate::App;
use crate::native_updater_service::ApplyMode;

pub(super) fn prelaunch_record(
    attempt_id: u64,
    mode: ApplyMode,
) -> (
    crate::HandoffPrelaunch,
    std::sync::mpsc::Receiver<()>,
    std::sync::mpsc::Receiver<super::HandoffStandDown>,
    std::sync::mpsc::Receiver<super::HandoffTransferJob>,
) {
    let (cancel, cancelled) = std::sync::mpsc::sync_channel(1);
    let (stand_down, stood_down) = std::sync::mpsc::sync_channel(1);
    let (transfer, transferred) = std::sync::mpsc::sync_channel(1);
    (
        crate::HandoffPrelaunch {
            stand_down_ack: std::sync::mpsc::sync_channel(1).0,
            attempt_id,
            nonce: "0123456789abcdef0123456789abcdef".to_string(),
            mode,
            apply_attempt: None,
            same_image: Some(super::SameImageHandoff::DebugSeam),
            target_build: crate::build_info::build_number().parse().unwrap_or(0),
            target_commit: crate::build_info::git_commit().to_string(),
            cancel,
            stand_down,
            transfer,
            arbiter: crate::HandoffAttemptArbiter::new(),
            launched_at: std::time::Instant::now(),
            dialled: None,
            park_retry_at: None,
            park_misses: 0,
            park_mid_sequence_reparks: 0,
            freeze_seed: super::FreezeSeed::Default,
            land_waits: 0,
            last_wait: None,
            stood_down: false,
            teardown: crate::DeferredHandoffTeardown::None,
            revoked_by_activity: false,
            history_export: None,
        },
        cancelled,
        stood_down,
        transferred,
    )
}

fn parked_record(attempt_id: u64, mode: ApplyMode) -> crate::PendingUpdateHandoff {
    let (cancel, _cancelled) = std::sync::mpsc::sync_channel(1);
    crate::PendingUpdateHandoff {
        park_at: std::time::Instant::now(),
        proof_ready_at: None,
        attempt_id,
        nonce: None,
        live: Vec::new(),
        adoption: Vec::new(),
        child_pid: None,
        mode,
        apply_attempt: None,
        same_image: Some(super::SameImageHandoff::DebugSeam),
        target_build: 0,
        target_commit: String::new(),
        layout: crate::restore::RestoreManifest::new(Vec::new()),
        layout_digest: [0; 32],
        screen_digest: [0; 32],
        activity_epoch: 0,
        hold_serials: 0,
        carried_claims: Vec::new(),
        cancel,
        arbiter: crate::HandoffAttemptArbiter::new(),
        teardown: crate::DeferredHandoffTeardown::None,
        commit_drain_started: None,
        commit_flush: None,
        held_commit: None,
        revoked_by_activity: false,
    }
}

fn completion(attempt_id: u64) -> crate::UpdateHandoffCompletion {
    crate::UpdateHandoffCompletion {
        attempt_id,
        nonce: None,
        child_pid: None,
        outcome: crate::UpdateHandoffOutcome::ActivityRevoked,
        commit_fd: None,
        reject: None,
        reconcile: None,
        detail: "stood down".to_string(),
        input_drain_spins: 0,
        child_death: crate::ChildDeathEvidence::Unobserved,
    }
}

/// A completion for an attempt that only ever PRELAUNCHED clears that
/// record, so a retry may launch.
#[test]
fn a_prelaunched_attempt_is_cleared_by_its_completion() {
    let mut app = App::headless_for_test();
    let (record, _cancelled, _stood_down, _transferred) = prelaunch_record(9, ApplyMode::Immediate);
    app.update_handoff_prelaunch = Some(record);
    assert!(app.update_handoff_in_flight());
    let teardown = app
        .reduce_returned_handoff_completion(completion(9))
        .expect("the prelaunched attempt is reduced");
    assert_eq!(teardown, crate::DeferredHandoffTeardown::None);
    assert!(app.update_handoff_prelaunch.is_none());
    assert!(app.pending_update_handoff.is_none());
    assert!(!app.update_handoff_in_flight());
}

/// A health warning is not the live attempt's refusal: one landing while a
/// prelaunched attempt is in flight leaves the Installing row and the rim; the
/// attempt's own completion retires them.
#[test]
fn a_health_warning_during_an_attempt_leaves_its_installing_row_and_rim() {
    use crate::messages_host::FlowPhase;
    let mut app = App::headless_for_test();
    app.begin_update_installing(true);
    let id = app
        .messages
        .live_by_key(crate::update_words::KEY_PROGRESS)
        .expect("the explicit apply's row")
        .id;
    let (record, _cancelled, _stood_down, _transferred) = prelaunch_record(9, ApplyMode::Immediate);
    app.update_handoff_prelaunch = Some(record);
    // The readers parked (the rim's start: round eight, leads U60 and U40).
    app.charge_upgrade_rim_at_park(41);
    assert!(app.note_update_health(
        aterm_update::health_failing_title("manifest"),
        "3 failed checks in a row since 2026-09-28T00:00:00Z: x",
    ));
    assert!(app.messages.live(id).is_some(), "the install is in flight");
    assert!(matches!(
        app.live_update_flow().map(|f| f.phase),
        Some(FlowPhase::Installing)
    ));
    assert!(app.level_up.is_some(), "the rim still explains the freeze");
    // The attempt ends without taking over: ITS completion retires both.
    app.reduce_returned_handoff_completion(completion(9));
    assert!(app.messages.live(id).is_none());
    assert!(app.level_up.is_none());
}

/// A flood's mid-sequence park never stands the successor down on its own:
/// through the production miss handler, a mid-sequence capture re-parks at the
/// 20 ms cadence without spending a rung for the whole mid-sequence bound, then
/// the ordinary ladder takes over, so a parser that stays mid-sequence ends in
/// the bounded stand-down, never an endless loop.
#[test]
fn a_mid_sequence_park_miss_re_parks_on_the_same_rung_without_standing_down() {
    let mut app = App::headless_for_test();
    let (record, _cancelled, stood_down, _transferred) =
        prelaunch_record(9, ApplyMode::AutomaticPastGrace);
    app.update_handoff_prelaunch = Some(record);
    let now = std::time::Instant::now();
    let reason = || {
        "session 1's parser was parked mid-sequence (csi-param) with the rest of that \
         sequence still queued on its PTY"
            .to_string()
    };
    for n in 1..=super::PRELAUNCH_MAX_MID_SEQUENCE_REPARKS {
        let (misses, reparks) = app
            .update_handoff_prelaunch
            .as_ref()
            .map(|p| (p.park_misses, p.park_mid_sequence_reparks))
            .expect("still prelaunched");
        app.dispose_park_miss(
            now,
            super::mid_sequence_miss_disposition(misses, reparks, reason()),
        );
        let p = app
            .update_handoff_prelaunch
            .as_ref()
            .expect("still holding");
        assert!(!p.stood_down, "re-park {n}: the successor keeps holding");
        assert_eq!(p.park_misses, 0, "re-park {n}: no freeze rung spent");
        assert_eq!(p.park_mid_sequence_reparks, n);
        assert_eq!(
            p.park_retry_at,
            Some(now + super::PRELAUNCH_PARK_RETRY),
            "re-park {n}: at the next 20 ms re-run, not the 500 ms rung delay"
        );
    }
    assert!(stood_down.try_recv().is_err(), "nothing was stood down");
    // Past the bound: the ordinary ladder, which re-parks first.
    app.dispose_park_miss(
        now,
        super::mid_sequence_miss_disposition(
            0,
            super::PRELAUNCH_MAX_MID_SEQUENCE_REPARKS,
            reason(),
        ),
    );
    let p = app
        .update_handoff_prelaunch
        .as_ref()
        .expect("still holding");
    assert_eq!(p.park_misses, 1, "past the bound a miss climbs the ladder");
    assert!(!p.stood_down);
}

/// A flood's batch boundaries fit the free re-park bound: an SGR-dense stream cut
/// at arbitrary reader boundaries leaves the parser mid-sequence at some cuts
/// (`Terminal::partial_sequence_state`) and at ground at others, and the longest
/// mid-sequence run is well inside the bound. Non-vacuous both ways.
#[test]
fn a_flood_cut_at_batch_boundaries_parks_well_inside_the_mid_sequence_bound() {
    let mut app = App::headless_for_test();
    app.push_stub_tab(crate::WindowId(0), crate::stub_session(app.next_session_id));
    // A coloured build log: every word in its own 256-colour SGR, varying
    // widths so no cut period aligns with the stream's.
    let mut flood = Vec::new();
    for i in 0..160_000_u32 {
        flood.extend_from_slice(
            format!(
                "\x1b[38;5;{}m{}\x1b[0m ",
                i % 256,
                "x".repeat((i % 7) as usize + 1)
            )
            .as_bytes(),
        );
        if i % 13 == 0 {
            flood.extend_from_slice(b"\r\n");
        }
    }
    let (mut at_ground, mut mid, mut run, mut longest_run) = (0, 0, 0_u32, 0_u32);
    // Reader batches of assorted sizes, as the kernel hands them out.
    let batches = [4096_usize, 1024, 65_536, 777, 16_384, 3000];
    let mut offset = 0;
    let mut k = 0;
    while offset < flood.len() {
        let end = (offset + batches[k % batches.len()]).min(flood.len());
        k += 1;
        let session = app.pool.iter().next().expect("a session");
        let mut term = session.term.lock().unwrap_or_else(|p| p.into_inner());
        term.process(&flood[offset..end]);
        offset = end;
        if term.partial_sequence_state().is_some() {
            mid += 1;
            run += 1;
            longest_run = longest_run.max(run);
        } else {
            at_ground += 1;
            run = 0;
        }
    }
    assert!(mid > 0, "the flood is cut mid-sequence at some boundaries");
    assert!(at_ground > 0, "and at ground at others");
    assert!(
        longest_run < u32::from(super::PRELAUNCH_MAX_MID_SEQUENCE_REPARKS),
        "longest mid-sequence run {longest_run} must fit the free re-park bound \
         ({mid} mid, {at_ground} at ground)"
    );
}

/// A completion whose attempt matches NEITHER record is not reduced and
/// clears nothing: a stale completion can never retire a newer attempt.
#[test]
fn a_stale_completion_clears_no_record() {
    let mut app = App::headless_for_test();
    let (record, _cancelled, _stood_down, _transferred) = prelaunch_record(9, ApplyMode::Automatic);
    app.update_handoff_prelaunch = Some(record);
    assert!(
        app.reduce_returned_handoff_completion(completion(8))
            .is_none()
    );
    assert!(app.update_handoff_prelaunch.is_some());
}

/// Both records for one attempt: the parked one's facts win, its teardown
/// merges the prelaunch record's, and both are cleared by the one completion.
#[test]
fn both_records_of_one_attempt_are_cleared_together_and_their_teardowns_merge() {
    let mut app = App::headless_for_test();
    let (mut record, _cancelled, _stood_down, _transferred) =
        prelaunch_record(4, ApplyMode::Immediate);
    record.teardown = crate::DeferredHandoffTeardown::QuitRequested;
    app.update_handoff_prelaunch = Some(record);
    app.pending_update_handoff = Some(parked_record(4, ApplyMode::Immediate));
    let teardown = app
        .reduce_returned_handoff_completion(completion(4))
        .expect("reduced");
    assert_eq!(teardown, crate::DeferredHandoffTeardown::QuitRequested);
    assert!(app.update_handoff_prelaunch.is_none());
    assert!(app.pending_update_handoff.is_none());
}

/// `Wake::UpdateHandoffStandingDown`: the gate closes for good, the parked record
/// is released (readers resume), and the attempt record stays with the deferred
/// teardown, so nothing launches beside the successor being stood down.
#[test]
fn a_stand_down_announcement_closes_the_gate_and_releases_the_parked_record() {
    let mut app = App::headless_for_test();
    let (mut record, _cancelled, _stood_down, _transferred) =
        prelaunch_record(4, ApplyMode::Automatic);
    let (ack_tx, ack) = std::sync::mpsc::sync_channel(1);
    record.stand_down_ack = ack_tx;
    app.update_handoff_prelaunch = Some(record);
    let mut parked = parked_record(4, ApplyMode::Automatic);
    parked.teardown = crate::DeferredHandoffTeardown::QuitRequested;
    parked.revoked_by_activity = true;
    app.pending_update_handoff = Some(parked);
    app.answer_prelaunched_stand_down(3);
    assert!(
        app.pending_update_handoff.is_some(),
        "an announcement for another attempt releases nothing"
    );
    assert!(
        !app.update_handoff_prelaunch
            .as_ref()
            .expect("record")
            .stood_down,
        "and closes no gate"
    );
    app.answer_prelaunched_stand_down(4);
    assert!(app.pending_update_handoff.is_none());
    let prelaunch = app
        .update_handoff_prelaunch
        .as_ref()
        .expect("the attempt record outlives the release");
    assert!(
        prelaunch.stood_down,
        "the park gate is shut: nothing may park onto a successor being killed"
    );
    assert!(prelaunch.park_retry_at.is_none());
    assert_eq!(
        prelaunch.teardown,
        crate::DeferredHandoffTeardown::QuitRequested
    );
    assert!(prelaunch.revoked_by_activity);
    assert!(app.update_handoff_in_flight());
    // The worker is waiting on this answer before it revokes.
    assert!(ack.try_recv().is_ok(), "the worker is answered last");
}

/// A destructive intent against a held successor is no barrier (nothing parked
/// or granted): it proceeds and the worker is asked to stand down. Against a
/// parked attempt it remains a barrier.
#[test]
fn a_teardown_is_no_barrier_for_a_held_successor_but_stands_it_down() {
    let mut app = App::headless_for_test();
    let (record, cancelled, stood_down, _transferred) = prelaunch_record(2, ApplyMode::Automatic);
    app.update_handoff_prelaunch = Some(record);
    assert!(
        !app.defer_pending_update_handoff_teardown(crate::DeferredHandoffTeardown::QuitRequested)
    );
    assert!(
        app.update_handoff_prelaunch
            .as_ref()
            .expect("the record outlives the teardown")
            .stood_down,
        "the park gate shuts: nothing may park onto a cancelled attempt"
    );
    assert_eq!(
        stood_down
            .try_recv()
            .expect("a typed stand-down reaches the worker")
            .outcome,
        crate::UpdateHandoffOutcome::ActivityRevoked
    );
    assert!(
        cancelled.try_recv().is_err(),
        "the typed channel carried it; the raw poke is only the fallback"
    );
    app.pending_update_handoff = Some(parked_record(2, ApplyMode::Automatic));
    assert!(
        app.defer_pending_update_handoff_teardown(crate::DeferredHandoffTeardown::QuitRequested)
    );
}

/// The main thread's typed stand-down reaches the worker once; a second
/// stand-down for the same attempt is a no-op, and a worker that is past
/// its hold (channel full or gone) is reached through the cancel poke.
#[test]
fn a_stand_down_is_typed_once_and_falls_back_to_the_cancel_poke() {
    let mut app = App::headless_for_test();
    let (record, cancelled, stood_down, _transferred) = prelaunch_record(6, ApplyMode::Automatic);
    app.update_handoff_prelaunch = Some(record);
    app.stand_down_prelaunched_successor(super::HandoffStandDown {
        outcome: crate::UpdateHandoffOutcome::ActivityRevoked,
        detail: "first".to_string(),
    });
    app.stand_down_prelaunched_successor(super::HandoffStandDown {
        outcome: crate::UpdateHandoffOutcome::TimedOut,
        detail: "second".to_string(),
    });
    let received = stood_down
        .try_recv()
        .expect("the first stand-down is delivered");
    assert_eq!(received.detail, "first");
    assert!(stood_down.try_recv().is_err(), "the second is a no-op");
    assert!(cancelled.try_recv().is_err());
    // A full channel (the worker has not drained the first) falls back to
    // the cancel poke for a fresh attempt.
    let (record, cancelled, stood_down, _transferred) = prelaunch_record(7, ApplyMode::Automatic);
    app.update_handoff_prelaunch = Some(record);
    app.update_handoff_prelaunch
        .as_ref()
        .expect("record")
        .stand_down
        .try_send(super::HandoffStandDown {
            outcome: crate::UpdateHandoffOutcome::Rejected,
            detail: "occupying the slot".to_string(),
        })
        .expect("the slot was free");
    app.stand_down_prelaunched_successor(super::HandoffStandDown {
        outcome: crate::UpdateHandoffOutcome::ActivityRevoked,
        detail: "cannot be delivered".to_string(),
    });
    assert!(
        cancelled.try_recv().is_ok(),
        "the cancel poke is the fallback"
    );
    drop(stood_down);
}

/// The park cue for the wrong attempt is ignored; the right one records the dial
/// and runs the gate, which with no session to park stands the attempt down as
/// activity-revoked.
#[test]
fn the_park_cue_binds_to_its_attempt_and_an_empty_pool_stands_down() {
    let mut app = App::headless_for_test();
    app.pool.sessions.clear();
    let (record, _cancelled, stood_down, _transferred) = prelaunch_record(11, ApplyMode::Immediate);
    app.update_handoff_prelaunch = Some(record);
    app.on_update_handoff_awaiting_park(10, Some(4242), None);
    assert!(
        app.update_handoff_prelaunch
            .as_ref()
            .is_some_and(|prelaunch| prelaunch.dialled.is_none()),
        "a cue for another attempt records no dial"
    );
    app.on_update_handoff_awaiting_park(11, Some(4242), None);
    let prelaunch = app.update_handoff_prelaunch.as_ref().expect("record");
    assert_eq!(
        prelaunch.dialled.and_then(|dialled| dialled.pid),
        Some(4242)
    );
    assert!(prelaunch.stood_down, "nothing to park is a stand-down");
    let received = stood_down.try_recv().expect("typed stand-down");
    assert_eq!(
        received.outcome,
        crate::UpdateHandoffOutcome::ActivityRevoked
    );
}

/// The headless fixture's session has no real master: the descriptor duplicate
/// fails, the parked readers roll back, and the held successor is stood down as
/// this process's failure (`ProducerFailed`, Transient), never as the candidate's
/// bytes failing.
#[test]
fn a_park_half_failure_stands_the_held_successor_down_as_a_producer_failure() {
    let mut app = App::headless_for_test();
    let (record, _cancelled, stood_down, transferred) = prelaunch_record(12, ApplyMode::Immediate);
    app.update_handoff_prelaunch = Some(record);
    app.on_update_handoff_awaiting_park(12, Some(4242), None);
    let prelaunch = app.update_handoff_prelaunch.as_ref().expect("record");
    assert!(prelaunch.stood_down);
    assert!(app.pending_update_handoff.is_none(), "nothing stays parked");
    assert!(transferred.try_recv().is_err(), "no capture was sent");
    let received = stood_down.try_recv().expect("typed stand-down");
    assert_eq!(
        received.outcome,
        crate::UpdateHandoffOutcome::ProducerFailed
    );
    assert!(
        received
            .detail
            .starts_with("could not reserve child-only PTY descriptors ("),
        "the descriptor failure carries its io::ErrorKind: {}",
        received.detail
    );
}

/// The post-park proof deadline ladder: short on the automatic lane, wider after
/// a physical failure of the same bytes, widest for an explicit apply.
#[test]
fn the_post_park_proof_deadline_widens_on_retries_and_for_an_explicit_apply() {
    if std::env::var_os("ATERM_HANDOFF_PROOF_TIMEOUT_MS").is_some() {
        return;
    }
    let park_at = std::time::Instant::now();
    let budget = |mode, prior| {
        super::handoff_proof_deadline(mode, prior, park_at).saturating_duration_since(park_at)
    };
    assert_eq!(
        budget(ApplyMode::Automatic, 0),
        std::time::Duration::from_secs(3)
    );
    assert_eq!(
        budget(ApplyMode::AutomaticPastGrace, 1),
        std::time::Duration::from_secs(6)
    );
    assert_eq!(
        budget(ApplyMode::Immediate, 0),
        std::time::Duration::from_secs(30)
    );
    assert!(matches!(
        crate::update_handoff_wake_class(&crate::Wake::UpdateHandoffAwaitingPark {
            attempt_id: 1,
            dialer_pid: None,
            grant_limit: None,
        }),
        crate::UpdateHandoffEventClass::Exempt
    ));
    assert!(matches!(
        crate::update_handoff_wake_class(&crate::Wake::UpdateHandoffStandingDown { attempt_id: 1 }),
        crate::UpdateHandoffEventClass::Exempt
    ));
}

/// The record never leaves an "installing" that was not: a switch on record then
/// stood down with this process running is written as stopped, once; an attempt
/// that never reached the record writes nothing.
#[test]
fn a_switch_on_record_that_stands_down_is_recorded_as_stopped() {
    let titles = |app: &App| {
        app.messages
            .log()
            .records()
            .map(|record| record.title.clone())
            .filter(|title| title.contains("reload") || title.contains("Reloading"))
            .collect::<Vec<_>>()
    };
    let mut app = App::headless_for_test();
    // Negative control: an attempt that never reached the record.
    let (record, _cancelled, _stood_down, _transferred) = prelaunch_record(8, ApplyMode::Automatic);
    app.update_handoff_prelaunch = Some(record);
    let _ = app.reduce_returned_handoff_completion(completion(8));
    assert!(titles(&app).is_empty(), "{:?}", titles(&app));

    app.record_update_switch_started(91, true, false);
    assert_eq!(titles(&app), vec!["Reloading aterm in place"]);
    let (record, _cancelled, _stood_down, _transferred) = prelaunch_record(9, ApplyMode::Automatic);
    app.update_handoff_prelaunch = Some(record);
    let _ = app.reduce_returned_handoff_completion(completion(9));
    assert_eq!(
        titles(&app),
        vec!["Reloading aterm in place", "Waiting to reload aterm"],
        "stood down on activity: routine, and said so"
    );
    // Once: the attempt is off the record now.
    app.record_update_switch_stopped(false, "again");
    assert_eq!(titles(&app).len(), 2);
}

/// A halt that moves after the park refuses the Commit: the successor adopts the
/// park's manifest while the control thread keeps serving `hold`, so a `hold on`
/// answered in between would reach no one. `exact_activity` requires every
/// session's hold serial to be where the park read it; a hold set and lifted in
/// the window moves it too.
#[test]
fn a_hold_that_moves_after_the_park_refuses_the_commit() {
    use crate::fabric::{Hold, apply_hold_for_test};
    let mut app = App::headless_for_test();
    let ctx = app
        .pool
        .iter()
        .next()
        .expect("the boot session")
        .ctx
        .clone();
    let arm = |app: &mut App| {
        let mut pending = parked_record(9, ApplyMode::Immediate);
        pending.live = app
            .pool
            .iter()
            .map(|session| (session.id, session.master, session.pid))
            .collect();
        pending.layout = app.capture_handoff_layout();
        pending.activity_epoch = app.update_handoff_activity_epoch;
        pending.hold_serials = app.hold_serials();
        app.pending_update_handoff = Some(pending);
    };
    let exact_activity = |app: &mut App| {
        app.collect_handoff_commit_facts(None, true, true, true, None, None)
            .expect("an attempt is pending")
            .0
            .exact_activity
    };
    let halt = || Hold {
        reason: "stop".to_string(),
        origin: "local".to_string(),
    };

    arm(&mut app);
    assert!(
        exact_activity(&mut app),
        "control: nothing moved since the park"
    );

    assert!(apply_hold_for_test(&ctx, Some(halt())));
    assert!(
        !exact_activity(&mut app),
        "a halt set after the park is not in the manifest: no Commit"
    );

    // Re-armed with the halt standing, a halt set and lifted inside the window
    // still refuses: the manifest carried a hold the session no longer has.
    arm(&mut app);
    assert!(exact_activity(&mut app));
    assert!(apply_hold_for_test(&ctx, None));
    assert!(apply_hold_for_test(&ctx, Some(halt())));
    assert!(!exact_activity(&mut app), "a round trip is still a move");

    // A no-op act (the hold already stands) moves nothing.
    arm(&mut app);
    assert!(!apply_hold_for_test(&ctx, Some(halt())));
    assert!(exact_activity(&mut app));
}

/// The turn-id ceiling belongs to the attempt: the Commit stands down only when
/// this process minted past the count its own manifest carried, not another
/// attempt's.
///
/// Robust to concurrent minting: the count only rises, so a ceiling of 1 under a
/// count of 2 or more is always passed, and one at `MAX_TURN_ID` never is.
#[test]
fn the_commit_reads_the_turn_id_ceiling_of_its_own_attempt() {
    let mut app = App::headless_for_test();
    crate::control::raise_turn_ids(2);
    let arm = |app: &mut App, ceiling: u64| {
        let mut pending = parked_record(9, ApplyMode::Immediate);
        pending.live = app
            .pool
            .iter()
            .map(|session| (session.id, session.master, session.pid))
            .collect();
        pending.layout = app.capture_handoff_layout();
        pending.activity_epoch = app.update_handoff_activity_epoch;
        pending.hold_serials = app.hold_serials();
        pending.arbiter.set_turn_id_ceiling(ceiling);
        app.pending_update_handoff = Some(pending);
    };
    let exact_activity = |app: &mut App| {
        app.collect_handoff_commit_facts(None, true, true, true, None, None)
            .expect("an attempt is pending")
            .0
            .exact_activity
    };

    // Another attempt's manifest set a low ceiling: this one never wrote
    // a manifest, so it has none, and nothing stands it down.
    let _other = crate::control::turn_ids_for_handoff();
    arm(&mut app, 0);
    assert!(exact_activity(&mut app), "no manifest, no ceiling");

    arm(&mut app, crate::handoff_carry::MAX_TURN_ID);
    assert!(exact_activity(&mut app), "the reserve held");

    arm(&mut app, 1);
    assert!(
        !exact_activity(&mut app),
        "negative control: minted past this attempt's ceiling, no Commit"
    );
}

/// A halt issued while the Commit runs is never answered and then lost. The
/// Commit's hold-serial reading raises a fence: a `hold on` issued after it gets
/// no answer while the attempt can still land, and lands (`OK hold=1`) the moment
/// the attempt stands down. A landed Commit `_exit`s with the caller waiting, so
/// its connection closes with no `OK`. Past the verb's bound it is `ERR busy`.
#[test]
fn a_hold_issued_during_the_commit_waits_for_the_attempt() {
    use crate::fabric::{HOLD_BUSY, HoldIssuer, cmd_hold, cmd_hold_within};
    let mut app = App::headless_for_test();
    let ctx = app
        .pool
        .iter()
        .next()
        .expect("the boot session")
        .ctx
        .clone();
    let sid = ctx.self_id.as_str().to_string();
    let mut pending = parked_record(9, ApplyMode::Immediate);
    pending.live = app
        .pool
        .iter()
        .map(|session| (session.id, session.master, session.pid))
        .collect();
    pending.layout = app.capture_handoff_layout();
    pending.activity_epoch = app.update_handoff_activity_epoch;
    pending.hold_serials = app.hold_serials();
    let parked_serials = pending.hold_serials;
    app.pending_update_handoff = Some(pending);

    let (facts, _, _, _, fence) = app
        .collect_handoff_commit_facts(None, true, true, true, None, None)
        .expect("an attempt is pending");
    assert!(
        facts.exact_activity,
        "control: nothing moved since the park"
    );

    // Past the bound, a committing session answers busy and moves nothing.
    assert_eq!(
        cmd_hold_within(
            &app.store,
            &format!("{sid} on reason=stop"),
            HoldIssuer::Owner,
            std::time::Duration::from_millis(50),
        ),
        HOLD_BUSY
    );
    assert_eq!(ctx.fabric.hold(), None);
    assert_eq!(app.hold_serials(), parked_serials);

    // Inside the bound, the act waits for the attempt.
    let store = app.store.clone();
    let line = format!("{sid} on reason=stop");
    let (tx, rx) = std::sync::mpsc::channel();
    std::thread::spawn(move || {
        let _ = tx.send(cmd_hold(&store, &line, HoldIssuer::Owner));
    });
    assert!(
        rx.recv_timeout(std::time::Duration::from_millis(300))
            .is_err(),
        "no answer while the Commit that read the serials can still land"
    );
    assert_eq!(ctx.fabric.hold(), None, "and nothing applied");
    assert_eq!(app.hold_serials(), parked_serials);

    // The attempt stands down: the halt lands and is answered.
    drop(fence);
    assert_eq!(
        rx.recv_timeout(aterm_spec::wait::MUST_HAPPEN)
            .expect("answered once the fence lifts"),
        "OK hold=1\n"
    );
    assert!(ctx.fabric.hold().is_some());
    assert_ne!(app.hold_serials(), parked_serials);
}

/// Round eight of the update audit, finding 751: what a record carries that
/// a control verb can move after the park's draw — a topic opt-in, a keyed
/// escalation, a supervisor claim — and that no other Commit fact compares,
/// stands the Commit down when it moved (a lossless retry, whose next draw
/// carries it). A read moves nothing, nor does a lease its own holder
/// renews.
#[test]
fn a_topic_escalation_or_claim_moved_after_the_draw_stands_the_commit_down() {
    use crate::session_timeline::{MetaEdit, claim_supervisor, write_attention_owned};
    let mut app = App::headless_for_test();
    let ctx = app
        .pool
        .iter()
        .next()
        .expect("the boot session")
        .ctx
        .clone();
    let arm = |app: &mut App| {
        let mut pending = parked_record(9, ApplyMode::Immediate);
        pending.live = app
            .pool
            .iter()
            .map(|session| (session.id, session.master, session.pid))
            .collect();
        pending.layout = app.capture_handoff_layout();
        pending.activity_epoch = app.update_handoff_activity_epoch;
        pending.hold_serials = app.hold_serials();
        let manifest = crate::session_store::SessionHandoff::from_store(
            &app.store.read().expect("the registry"),
        );
        pending.carried_claims =
            super::carried_claims_of(&super::handed_sessions_only(manifest, &pending.live));
        assert_eq!(pending.carried_claims.len(), 1, "the boot session is drawn");
        app.pending_update_handoff = Some(pending);
    };
    let exact_activity = |app: &mut App| {
        app.collect_handoff_commit_facts(None, true, true, true, None, None)
            .expect("an attempt is pending")
            .0
            .exact_activity
    };
    arm(&mut app);
    assert!(exact_activity(&mut app), "control: nothing moved");
    // Negative control: reads move nothing.
    assert!(crate::fabric::cmd_topic(&ctx, "ls").0.starts_with("OK 0"));
    let _ = ctx
        .meta
        .lock()
        .expect("the meta")
        .handoff_attention_owners();
    assert!(exact_activity(&mut app), "a read is no change");

    // `topic add` answered after the draw: the Commit stands down, and the
    // next attempt, which draws it, commits.
    let (reply, _) = crate::fabric::cmd_topic(&ctx, "add news");
    assert!(reply.starts_with("OK news since=head added=1"), "{reply}");
    assert!(!exact_activity(&mut app), "the opt-in is in no manifest");
    arm(&mut app);
    assert!(exact_activity(&mut app), "drawn again, it is carried");
    let (reply, _) = crate::fabric::cmd_topic(&ctx, "drop news");
    assert!(reply.starts_with("OK news dropped=1"), "{reply}");
    assert!(!exact_activity(&mut app), "nor is the drop");
    arm(&mut app);

    // A keyed escalation (the bare one rides the layout, compared apart).
    write_attention_owned(&ctx, "ci", MetaEdit::Set("needs your review"))
        .expect("a keyed escalation");
    assert!(
        !exact_activity(&mut app),
        "the escalation is in no manifest"
    );
    arm(&mut app);
    assert!(exact_activity(&mut app));

    // A supervisor's lease, and its holder renewing it.
    let now = crate::metrics::now_us();
    claim_supervisor(&ctx, "sup-a", None, Some(now + 60_000_000), now)
        .expect("an unclaimed session");
    assert!(!exact_activity(&mut app), "the claim is in no manifest");
    arm(&mut app);
    assert!(exact_activity(&mut app));
    let now = crate::metrics::now_us();
    claim_supervisor(&ctx, "sup-a", None, Some(now + 120_000_000), now).expect("a renewal");
    assert!(
        exact_activity(&mut app),
        "a lease its holder renews carries the same holder"
    );
}

/// A `topic` change issued while the Commit's fence stands waits for the
/// attempt, as a `hold` does: the Commit compared the set under the fence,
/// so a change answered after it would be lost with this process.
#[test]
fn a_topic_change_issued_during_the_commit_waits_for_the_attempt() {
    let mut app = App::headless_for_test();
    let ctx = app
        .pool
        .iter()
        .next()
        .expect("the boot session")
        .ctx
        .clone();
    let mut pending = parked_record(9, ApplyMode::Immediate);
    pending.live = app
        .pool
        .iter()
        .map(|session| (session.id, session.master, session.pid))
        .collect();
    pending.layout = app.capture_handoff_layout();
    pending.activity_epoch = app.update_handoff_activity_epoch;
    pending.hold_serials = app.hold_serials();
    app.pending_update_handoff = Some(pending);
    let (facts, _, _, _, fence) = app
        .collect_handoff_commit_facts(None, true, true, true, None, None)
        .expect("an attempt is pending");
    assert!(facts.exact_activity, "control: nothing moved");

    let (tx, rx) = std::sync::mpsc::channel();
    let waiting = std::sync::Arc::clone(&ctx);
    std::thread::spawn(move || {
        let _ = tx.send(crate::fabric::cmd_topic(&waiting, "add news").0);
    });
    assert!(
        rx.recv_timeout(std::time::Duration::from_millis(300))
            .is_err(),
        "no answer while the Commit that compared the set can still land"
    );
    assert!(ctx.fabric.topics().is_empty(), "and nothing applied");

    // The attempt stands down: the opt-in lands and is answered.
    drop(fence);
    let reply = rx
        .recv_timeout(aterm_spec::wait::MUST_HAPPEN)
        .expect("answered once the fence lifts");
    assert!(reply.starts_with("OK news since=head added=1"), "{reply}");
    assert_eq!(ctx.fabric.topics().len(), 1);
}
