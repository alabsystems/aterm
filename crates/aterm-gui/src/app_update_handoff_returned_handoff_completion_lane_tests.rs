// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use std::time::Duration;

use crate::App;
use crate::native_updater_service::{
    ApplyAttemptTicket, ApplyMode, CheckCompletion, CheckStart, DurableUpdateStatus,
};

const TEST_COMMIT: &str = "0123456789abcdef0123456789abcdef01234567";

/// Stage one strictly-newer build through the real check reducer.
fn stage_one_build(app: &mut App) -> u64 {
    let current_build = app.native_updater_service.snapshot().current_build;
    let build = current_build + 1;
    let CheckStart::Start(ticket) = app.native_updater_service.request_check() else {
        panic!("a fresh service must start exactly one check");
    };
    assert_eq!(
        app.native_updater_service.finish_check(
            ticket,
            DurableUpdateStatus {
                linux_host: false,
                linux: None,
                enabled: true,
                current_build,
                staged_build: Some(build),
                staged_version: Some(format!("1.0.{build}")),
                staged_commit: Some(TEST_COMMIT.to_string()),
                staged_dmg_sha256: Some("ab".repeat(32)),
                changelog: None,
                outcome: "staged".to_string(),
                failing_checks: 0,
                failing_persistent: false,
                failing_kind: String::new(),
                failing_applies: 0,
                apply_failure: String::new(),
                apply_failure_build: 0,
                apply_failures_for_target: 0,
                installable: Some(true),
                channel_unreadable: false,
                checked_at: None,
            },
        ),
        CheckCompletion::Reduced,
        "PRECONDITION: the check must reduce, or nothing is staged"
    );
    build
}

/// Whether a latch deadline is a structural verdict's single re-sample a day out.
fn is_structural_resample(retry_at: Option<std::time::Instant>) -> bool {
    retry_at.is_some_and(|at| {
        let wait = at.saturating_duration_since(std::time::Instant::now());
        wait > crate::app_native::STRUCTURAL_RESAMPLE_AFTER - std::time::Duration::from_secs(60)
            && wait <= crate::app_native::STRUCTURAL_RESAMPLE_AFTER
    })
}

/// Drive one returned handoff for `mode` through `reduce_returned_handoff_completion`.
fn reduce_one_returned_failure(
    app: &mut App,
    mode: ApplyMode,
    build: u64,
    digest: &str,
    outcome: crate::UpdateHandoffOutcome,
    child_death: crate::ChildDeathEvidence,
) {
    let ticket = ApplyAttemptTicket::for_test(build, TEST_COMMIT, digest);
    ticket.make_current_apply_for_test(&mut app.native_updater_service);
    let (cancel, _cancelled) = std::sync::mpsc::sync_channel(1);
    app.pending_update_handoff = Some(crate::PendingUpdateHandoff {
        park_at: std::time::Instant::now(),
        proof_ready_at: None,
        attempt_id: 1,
        nonce: None,
        live: Vec::new(),
        adoption: Vec::new(),
        child_pid: None,
        mode,
        apply_attempt: Some(ticket),
        same_image: None,
        target_build: build,
        target_commit: TEST_COMMIT.to_string(),
        layout: crate::restore::RestoreManifest::new(Vec::new()),
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
    let teardown = app
        .reduce_returned_handoff_completion(crate::UpdateHandoffCompletion {
            attempt_id: 1,
            nonce: None,
            child_pid: None,
            outcome,
            commit_fd: None,
            reject: None,
            reconcile: None,
            detail: format!("handoff proof ended {outcome:?}"),
            input_drain_spins: 0,
            child_death,
        })
        .expect("a matching pending attempt is always reduced");
    assert_eq!(
        teardown,
        crate::DeferredHandoffTeardown::None,
        "PRECONDITION: this fixture requests no structural teardown, so the \
         replay branch cannot be what changed the state asserted on"
    );
}

#[test]
fn a_person_s_returned_handoff_never_spends_the_automatic_lane_s_budget() {
    let _ledger = crate::app_update_screen::hold_update_ledger_for_test();
    let mut app = App::headless_for_test();
    let build = stage_one_build(&mut app);
    assert!(
        app.arm_native_auto_apply(build, &"ab".repeat(32)),
        "PRECONDITION: the background lane is armed for these exact bytes, \
         which is the state a person's click has to leave alone"
    );

    reduce_one_returned_failure(
        &mut app,
        ApplyMode::Immediate,
        build,
        &"ab".repeat(32),
        crate::UpdateHandoffOutcome::TimedOut,
        crate::ChildDeathEvidence::Unobserved,
    );
    assert_eq!(
        app.auto_apply_physical_retry.map(|retry| retry.cycles),
        None,
        "a person-initiated handoff failure must not open (or advance) the \
         automatic artifact's converging physical budget"
    );
    assert!(
        app.auto_apply_manual_only.is_none(),
        "a person's failure must not latch automatic apply off — a manual \
         retry that converges the background lane is the finding"
    );
    assert!(
        app.auto_apply_intent
            .is_some_and(|intent| intent.build == build),
        "and it must not retire the live automatic intent either"
    );

    reduce_one_returned_failure(
        &mut app,
        ApplyMode::AutomaticPastGrace,
        build,
        &"ab".repeat(32),
        crate::UpdateHandoffOutcome::TimedOut,
        crate::ChildDeathEvidence::Unobserved,
    );
    assert_eq!(
        app.auto_apply_physical_retry.map(|retry| retry.cycles),
        Some(1),
        "the automatic lane's own failure is what the budget is for"
    );
    let latched = app
        .auto_apply_manual_only
        .expect("an automatic physical failure latches manual-only")
        .retry_at
        .expect("with a deadline, on its first failure")
        .saturating_duration_since(std::time::Instant::now());
    assert!(
        latched > std::time::Duration::from_secs(500)
            && latched <= std::time::Duration::from_secs(600),
        "and on the physical schedule's first rung (~600s), got {latched:?}"
    );
    assert!(
        app.auto_apply_intent.is_none(),
        "an automatic physical failure retires the intent it was spent on"
    );
}

#[test]
fn a_person_s_activity_revoked_completion_arms_no_automatic_retry() {
    let _ledger = crate::app_update_screen::hold_update_ledger_for_test();
    let mut app = App::headless_for_test();
    let build = stage_one_build(&mut app);
    let ticket = ApplyAttemptTicket::for_test(build, TEST_COMMIT, &"ab".repeat(32));
    ticket.make_current_apply_for_test(&mut app.native_updater_service);
    let (cancel, _cancelled) = std::sync::mpsc::sync_channel(1);
    app.pending_update_handoff = Some(crate::PendingUpdateHandoff {
        park_at: std::time::Instant::now(),
        proof_ready_at: None,
        attempt_id: 2,
        nonce: None,
        live: Vec::new(),
        adoption: Vec::new(),
        child_pid: None,
        mode: ApplyMode::Immediate,
        apply_attempt: Some(ticket),
        same_image: None,
        target_build: build,
        target_commit: TEST_COMMIT.to_string(),
        layout: crate::restore::RestoreManifest::new(Vec::new()),
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
        revoked_by_activity: true,
    });
    let _ = app.reduce_returned_handoff_completion(crate::UpdateHandoffCompletion {
        attempt_id: 2,
        nonce: None,
        child_pid: None,
        outcome: crate::UpdateHandoffOutcome::ActivityRevoked,
        commit_fd: None,
        reject: None,
        reconcile: None,
        detail: "activity revoked handoff during physical preparation".to_string(),
        input_drain_spins: 0,
        child_death: crate::ChildDeathEvidence::Unobserved,
    });
    assert!(
        app.auto_overlap_retry.is_none(),
        "a person's revoked attempt must not consume — or create — the \
         automatic artifact's activity-revoked retry budget"
    );
    assert!(
        app.auto_apply_manual_only.is_none(),
        "nor may it latch the background lane off"
    );
}

#[test]
fn a_switch_the_automatic_lane_retries_by_itself_is_recorded_as_waiting() {
    let _ledger = crate::app_update_screen::hold_update_ledger_for_test();
    let stops = |app: &App| {
        app.messages
            .log()
            .records()
            .filter(|record| {
                let detail = record.detail.join(" ");
                (record.title.starts_with("Couldn't install aterm")
                    || record.title.starts_with("Waiting to install aterm")
                    || record.title == "Update waiting")
                    && (detail.contains("kept running")
                        || detail.contains("move to the new version"))
            })
            .map(|record| {
                (
                    record.title.clone(),
                    record.severity,
                    record.detail.join(" "),
                )
            })
            .collect::<Vec<_>>()
    };
    for (mode, outcome, retried) in [
        (
            ApplyMode::AutomaticPastGrace,
            crate::UpdateHandoffOutcome::CaptureRefused,
            true,
        ),
        (
            ApplyMode::AutomaticPastGrace,
            crate::UpdateHandoffOutcome::TimedOut,
            true,
        ),
        (
            ApplyMode::Immediate,
            crate::UpdateHandoffOutcome::TimedOut,
            false,
        ),
    ] {
        let mut app = App::headless_for_test();
        let build = stage_one_build(&mut app);
        assert!(app.arm_native_auto_apply(build, &"ab".repeat(32)));
        let running = app.native_updater_service.snapshot().current_build;
        app.record_update_switch_started(running, false, mode.is_automatic());
        reduce_one_returned_failure(
            &mut app,
            mode,
            build,
            &"ab".repeat(32),
            outcome,
            crate::ChildDeathEvidence::Unobserved,
        );
        let stops = stops(&app);
        assert_eq!(stops.len(), 1, "{mode:?} {outcome:?}: {stops:?}");
        let (_, severity, _) = &stops[0];
        let expected = if retried {
            aterm_messages::Severity::Info
        } else {
            aterm_messages::Severity::Warn
        };
        assert_eq!(
            *severity, expected,
            "{mode:?} {outcome:?}: a lane that tries again by itself asks nothing \
             of the person; a stop does: {stops:?}"
        );
    }
}

#[test]
fn a_structural_worker_outcome_reaches_a_different_schedule_from_a_transient_one() {
    let _ledger = crate::app_update_screen::hold_update_ledger_for_test();
    let mut app = App::headless_for_test();
    let build = stage_one_build(&mut app);
    let deadline = |app: &App| {
        app.auto_apply_manual_only
            .expect("an automatic physical failure always latches manual-only")
            .retry_at
    };

    for _ in 0..2 {
        reduce_one_returned_failure(
            &mut app,
            ApplyMode::AutomaticPastGrace,
            build,
            &"ab".repeat(32),
            crate::UpdateHandoffOutcome::AdoptionMismatch,
            crate::ChildDeathEvidence::Unobserved,
        );
    }
    assert!(
        is_structural_resample(deadline(&app)),
        "two proofs that these two images disagree is not a busy afternoon; \
         the lane must be finished with the artifact's schedule — its one \
         re-sample a day out, not a third park/spawn/paint round trip inside \
         the hour: {:?}",
        deadline(&app)
    );

    for _ in 0..2 {
        reduce_one_returned_failure(
            &mut app,
            ApplyMode::AutomaticPastGrace,
            build,
            &"cd".repeat(32),
            crate::UpdateHandoffOutcome::TimedOut,
            crate::ChildDeathEvidence::Unobserved,
        );
    }
    let transient = deadline(&app)
        .expect("a transient failure two attempts in is still coming back")
        .saturating_duration_since(std::time::Instant::now());
    assert!(
        transient > std::time::Duration::from_secs(1700)
            && transient <= std::time::Duration::from_secs(1800),
        "the transient lane must be on its second rung (~1800s), got {transient:?}"
    );
}

#[test]
fn a_park_that_missed_every_rung_is_retried_as_activity_and_never_latches() {
    let _ledger = crate::app_update_screen::hold_update_ledger_for_test();
    let mut app = App::headless_for_test();
    let build = stage_one_build(&mut app);
    let armed_at = std::time::Instant::now()
        - crate::native_update_auto_intent::PREFER_IDLE_WINDOW
        - crate::native_update_auto_intent::PREFER_OUTPUT_GAP_WINDOW
        - std::time::Duration::from_secs(1);
    app.auto_apply_ladder = Some(crate::AutoApplyLadder {
        build,
        armed_at,
        announced: crate::native_update_auto_intent::ApplyPhase::KeysOnly,
        restored_hold_said: false,
    });
    for miss_pair in 1..=2 {
        let super::ParkMissDisposition::StandDown(stand_down) = super::park_miss_disposition(
            super::PRELAUNCH_MAX_PARK_MISSES,
            "a PTY reader missed the 250 ms handoff park deadline".to_string(),
        ) else {
            panic!("past the last rung the attempt stands down");
        };
        reduce_one_returned_failure(
            &mut app,
            ApplyMode::AutomaticPastGrace,
            build,
            &"ab".repeat(32),
            stand_down.outcome,
            crate::ChildDeathEvidence::Unobserved,
        );
        assert!(
            app.auto_apply_manual_only.is_none(),
            "miss pair {miss_pair}: a busy machine never latches the lane manual-only"
        );
        assert!(
            app.auto_apply_intent.is_some(),
            "miss pair {miss_pair}: the lane keeps a live retry intent"
        );
        assert!(
            app.auto_apply_physical_retry.is_none(),
            "miss pair {miss_pair}: no physical budget is spent on a stopwatch"
        );
        assert_eq!(
            app.auto_apply_ladder.map(|ladder| ladder.armed_at),
            Some(armed_at),
            "miss pair {miss_pair}: the ladder's anchor is untouched"
        );
    }
    assert_eq!(
        app.auto_overlap_retry.map(|retry| retry.cycles),
        Some(2),
        "each stood-down attempt costs one rung of the ACTIVITY spacing, which \
         saturates and never ends the lane"
    );
}

#[cfg(target_os = "macos")]
#[test]
fn a_capture_refusal_is_recorded_waits_for_the_desk_and_never_latches() {
    let _ledger = crate::app_update_screen::hold_update_ledger_for_test();
    let mut app = App::headless_for_test();
    let build = stage_one_build(&mut app);
    let dmg = [0xab_u8; 32];
    let armed_at = std::time::Instant::now() - std::time::Duration::from_secs(400);
    app.auto_apply_ladder = Some(crate::AutoApplyLadder {
        build,
        armed_at,
        announced: crate::native_update_auto_intent::ApplyPhase::KeysOnly,
        restored_hold_said: false,
    });
    let mut failing = None;
    for refusal in 1..=2_u32 {
        let before = std::time::Instant::now();
        reduce_one_returned_failure(
            &mut app,
            ApplyMode::AutomaticPastGrace,
            build,
            &"ab".repeat(32),
            crate::UpdateHandoffOutcome::CaptureRefused,
            crate::ChildDeathEvidence::Unobserved,
        );
        assert!(
            app.auto_apply_manual_only.is_none(),
            "refusal {refusal}: a capture refusal never latches the lane"
        );
        assert!(
            app.auto_apply_physical_retry.is_none(),
            "refusal {refusal}: it is not the candidate's bytes"
        );
        assert!(
            app.auto_overlap_retry.is_none(),
            "refusal {refusal}: it is not the machine being busy"
        );
        let intent = app.auto_apply_intent.expect("the lane keeps a live intent");
        assert_eq!((intent.build, intent.dmg_sha256), (build, dmg));
        assert!(
            intent.retry_at >= before + crate::app_native::CAPTURE_REFUSAL_PROBE,
            "refusal {refusal}: re-probed on the refusal lane's cadence"
        );
        assert!(app.auto_apply_capture_refusal.is_some());
        assert_eq!(
            app.auto_apply_ladder.map(|ladder| ladder.armed_at),
            Some(armed_at),
            "refusal {refusal}: the ladder's anchor is untouched"
        );
        let snapshot = app.native_updater_service.snapshot();
        assert_eq!(
            snapshot.failing_applies,
            failing.map_or(snapshot.failing_applies.max(1), |prior: u32| prior + 1),
            "refusal {refusal}: recorded as an apply failure, which is what moves \
             `failing_applies` toward the persistent-failure notice"
        );
        failing = Some(snapshot.failing_applies);
        assert!(
            snapshot.apply_failure.contains("CaptureRefused"),
            "`update status` names the refusal: {:?}",
            snapshot.apply_failure
        );
    }

    let now = std::time::Instant::now();
    assert!(app.capture_refusal_holds(build, dmg, false, now));
    assert!(app.capture_refusal_holds(build, dmg, false, now));
    assert!(!app.capture_refusal_holds(build + 1, dmg, false, now));
    for session in app.pool.iter() {
        crate::term_lock(&session.term).resize(30, 100);
    }
    let spaced = now + crate::app_native::capture_refusal_spacing(2);
    assert!(app.capture_refusal_holds(build, dmg, false, now));
    assert!(!app.capture_refusal_holds(build, dmg, false, spaced));
    assert!(
        !app.capture_refusal_holds(build, dmg, false, spaced),
        "a released refusal holds nothing more, so the attempt that follows is ordinary"
    );

    app.auto_apply_capture_refusal = Some(crate::AutoApplyCaptureRefusal {
        build,
        dmg_sha256: dmg,
        activation: false,
        desk: app.capture_desk(),
        refused_at: now,
        refusals: 1,
        released: false,
    });
    assert!(app.capture_refusal_holds(build, dmg, false, now));
    assert!(!app.capture_refusal_holds(
        build,
        dmg,
        false,
        now + crate::app_native::CAPTURE_REFUSAL_BACKSTOP
    ));
}

/// Arm the refusal lane for `build` through the production reducer.
fn refuse_capture(app: &mut App, build: u64) {
    reduce_one_returned_failure(
        app,
        ApplyMode::AutomaticPastGrace,
        build,
        &"ab".repeat(32),
        crate::UpdateHandoffOutcome::CaptureRefused,
        crate::ChildDeathEvidence::Unobserved,
    );
}

#[test]
fn a_parser_between_reads_does_not_release_a_capture_refusal() {
    let _ledger = crate::app_update_screen::hold_update_ledger_for_test();
    let mut app = App::headless_for_test();
    let build = stage_one_build(&mut app);
    let dmg = [0xab_u8; 32];
    let session = app.pool.iter().next().expect("a session").term.clone();
    crate::term_lock(&session).process(b"\x1b[38;5");
    refuse_capture(&mut app, build);
    let now = std::time::Instant::now();
    crate::term_lock(&session).process(b"1m");
    let probe = now + crate::app_native::CAPTURE_REFUSAL_PROBE + Duration::from_secs(1);
    assert!(
        app.capture_refusal_holds(build, dmg, false, probe),
        "the parser's ground state is not the desk"
    );
    crate::term_lock(&session).resize(30, 100);
    assert!(
        !app.capture_refusal_holds(build, dmg, false, probe),
        "a resize is"
    );
}

#[test]
fn a_capture_refusal_that_comes_back_is_spaced_not_retried_every_probe() {
    let _ledger = crate::app_update_screen::hold_update_ledger_for_test();
    let mut app = App::headless_for_test();
    let build = stage_one_build(&mut app);
    let dmg = [0xab_u8; 32];
    let mut rows = 30;
    let mut move_desk = |app: &mut App| {
        rows += 1;
        for session in app.pool.iter() {
            crate::term_lock(&session.term).resize(rows, 100);
        }
    };
    for refusal in 1..=4_u32 {
        let floor = crate::app_native::capture_refusal_spacing(refusal).as_secs();
        // The refusal is stamped somewhere between these two readings, however long the
        // reducer takes on a loaded machine: each assert reads from the side that bounds it.
        let before = std::time::Instant::now();
        refuse_capture(&mut app, build);
        let after = std::time::Instant::now();
        move_desk(&mut app);
        assert!(
            app.capture_refusal_holds(build, dmg, false, before + Duration::from_secs(floor - 2)),
            "refusal {refusal}: a changed desk is not attempted before {floor} s"
        );
        assert!(
            !app.capture_refusal_holds(build, dmg, false, after + Duration::from_secs(floor + 2)),
            "refusal {refusal}: a changed desk is attempted at {floor} s"
        );
        assert!(
            app.auto_apply_manual_only.is_none(),
            "refusal {refusal}: never a latch"
        );
    }
    refuse_capture(&mut app, build + 1);
    let refused_at = std::time::Instant::now();
    move_desk(&mut app);
    assert!(!app.capture_refusal_holds(
        build + 1,
        dmg,
        false,
        refused_at + crate::app_native::CAPTURE_REFUSAL_PROBE + Duration::from_secs(2)
    ));
}

#[test]
fn a_refusal_recorded_on_a_busy_desk_takes_its_baseline_at_the_probe() {
    let _ledger = crate::app_update_screen::hold_update_ledger_for_test();
    let mut app = App::headless_for_test();
    let build = stage_one_build(&mut app);
    let dmg = [0xab_u8; 32];
    refuse_capture(&mut app, build);
    let busy = HeldEngine::hold(&app.pool.iter().next().expect("a session").term);
    // The refusal is stamped between these two readings: a hold is read from the first, a
    // release from the second, so a slow reducer on a loaded machine moves neither.
    let before = std::time::Instant::now();
    app.arm_capture_refusal_retry(build, dmg, false, "the park's capture refused");
    let refused_at = std::time::Instant::now();
    drop(busy);
    let record = app
        .auto_apply_capture_refusal
        .clone()
        .expect("the refusal is on record");
    assert_eq!(record.refusals, 2, "PRECONDITION: a refusal that came back");
    assert!(
        !record.desk.complete(),
        "PRECONDITION: recorded while an engine was busy, so that session has no baseline yet"
    );
    let probe = crate::app_native::CAPTURE_REFUSAL_BASELINE_PROBE;
    let spacing = crate::app_native::capture_refusal_spacing(2);
    let intent = app.auto_apply_intent.expect("the lane keeps its intent");
    assert!(
        intent.retry_at <= refused_at + probe,
        "the lane comes back within {probe:?} for its baseline, not after the {spacing:?} \
         spacing"
    );
    assert!(app.capture_refusal_holds(build, dmg, false, refused_at + probe));
    assert!(
        app.auto_apply_capture_refusal
            .as_ref()
            .is_some_and(|refusal| refusal.desk.complete()),
        "the first readable desk is the baseline, inside the spacing"
    );
    assert!(
        app.capture_refusal_reprobe_at(refused_at + probe)
            >= before + spacing - Duration::from_secs(1)
    );
    for session in app.pool.iter() {
        crate::term_lock(&session.term).resize(31, 100);
    }
    assert!(
        app.capture_refusal_holds(build, dmg, false, before + spacing - Duration::from_secs(2)),
        "the spacing still holds the release"
    );
    assert!(
        !app.capture_refusal_holds(
            build,
            dmg,
            false,
            refused_at + spacing + Duration::from_secs(2)
        ),
        "a change made during the spacing releases it when the spacing ends, not at the \
         hourly backstop"
    );
}

/// A reader holding one session's engine, as a reader draining a flood does, until dropped.
struct HeldEngine {
    unlock: Option<std::sync::mpsc::Sender<()>>,
    reader: Option<std::thread::JoinHandle<()>>,
}

impl HeldEngine {
    fn hold(term: &std::sync::Arc<std::sync::Mutex<aterm_core::terminal::Terminal>>) -> Self {
        let term = std::sync::Arc::clone(term);
        let (locked, is_locked) = std::sync::mpsc::channel();
        let (unlock, unlocked) = std::sync::mpsc::channel::<()>();
        let reader = std::thread::spawn(move || {
            let _engine = crate::term_lock(&term);
            locked.send(()).expect("the test waits for the lock");
            let _ = unlocked.recv_timeout(Duration::from_secs(60));
        });
        is_locked.recv().expect("the reader holds the engine");
        Self {
            unlock: Some(unlock),
            reader: Some(reader),
        }
    }
}

impl Drop for HeldEngine {
    fn drop(&mut self) {
        drop(self.unlock.take());
        if let Some(reader) = self.reader.take() {
            let _ = reader.join();
        }
    }
}

/// A desk of three tabs whose first one a reader holds, and the two readings the second
/// refusal was stamped between (a hold is read from the first, a release from the second).
fn second_refusal_beside_a_busy_engine(
    app: &mut App,
) -> (
    u64,
    HeldEngine,
    [u64; 2],
    (std::time::Instant, std::time::Instant),
) {
    let build = stage_one_build(app);
    let dmg = [0xab_u8; 32];
    let busy_term = app.pool.iter().next().expect("a session").term.clone();
    let quiet = [app.next_session_id, app.next_session_id + 1];
    app.next_session_id += 2;
    for id in quiet {
        app.pool.insert(crate::stub_session(id));
    }
    app.arm_capture_refusal_retry(build, dmg, false, "the park's capture refused");
    let busy = HeldEngine::hold(&busy_term);
    let before = std::time::Instant::now();
    app.arm_capture_refusal_retry(build, dmg, false, "the park's capture refused");
    let refused_at = std::time::Instant::now();
    let record = app
        .auto_apply_capture_refusal
        .as_ref()
        .expect("the refusal is on record");
    assert_eq!(record.refusals, 2, "PRECONDITION: a refusal that came back");
    assert_eq!(
        record.desk.unread(),
        1,
        "PRECONDITION: only the busy tab's reading is missing"
    );
    (build, busy, quiet, (before, refused_at))
}

#[test]
fn a_tab_closed_or_resized_before_the_first_probe_beside_a_busy_engine_counts() {
    let _ledger = crate::app_update_screen::hold_update_ledger_for_test();
    let dmg = [0xab_u8; 32];
    let spacing = crate::app_native::capture_refusal_spacing(2);
    let after = |refused_at: std::time::Instant, secs: u64| {
        refused_at + spacing + Duration::from_secs(secs)
    };
    for change in ["close", "resize", "nothing"] {
        let mut app = App::headless_for_test();
        let (build, busy, quiet, (before, refused_at)) =
            second_refusal_beside_a_busy_engine(&mut app);
        match change {
            "close" => assert!(app.pool.detach(quiet[0]), "PRECONDITION: the tab closes"),
            "resize" => {
                crate::term_lock(&app.pool.get(quiet[1]).expect("an open tab").term)
                    .resize(31, 100);
            }
            _ => {}
        }
        for probe in [
            crate::app_native::CAPTURE_REFUSAL_BASELINE_PROBE,
            crate::app_native::CAPTURE_REFUSAL_PROBE,
            spacing - Duration::from_secs(2),
        ] {
            assert!(
                app.capture_refusal_holds(build, dmg, false, before + probe),
                "{change}: the spacing holds the release at {probe:?}"
            );
        }
        assert!(
            app.auto_apply_capture_refusal
                .as_ref()
                .is_some_and(|refusal| refusal.desk.unread() == 1),
            "PRECONDITION {change}: the busy tab was never read"
        );
        let released = !app.capture_refusal_holds(build, dmg, false, after(refused_at, 2));
        if change != "nothing" {
            assert!(
                released,
                "{change}: a tab changed before the first probe releases the refusal when \
                 the spacing ends, not at the hourly backstop"
            );
            continue;
        }
        assert!(
            !released,
            "nothing changed: a busy tab says nothing about itself"
        );
        drop(busy);
        assert!(
            app.capture_refusal_holds(build, dmg, false, after(refused_at, 4)),
            "the busy tab's first reading after the spacing is its baseline, not a change"
        );
        assert!(
            app.auto_apply_capture_refusal
                .as_ref()
                .is_some_and(|refusal| refusal.desk.complete()),
            "PRECONDITION: the busy tab's baseline was taken"
        );
        for session in app
            .pool
            .iter()
            .filter(|session| !quiet.contains(&session.id))
        {
            crate::term_lock(&session.term).resize(31, 100);
        }
        assert!(
            !app.capture_refusal_holds(build, dmg, false, after(refused_at, 6)),
            "a change of the once-busy tab after its baseline releases"
        );
    }
}

#[test]
fn a_poll_that_waits_for_activity_still_reads_a_busy_tabs_baseline() {
    let _ledger = crate::app_update_screen::hold_update_ledger_for_test();
    let mut app = App::headless_for_test();
    let (_build, busy, _quiet, (_, refused_at)) = second_refusal_beside_a_busy_engine(&mut app);
    let intent = app.auto_apply_intent.expect("the lane keeps its intent");
    assert!(
        intent.retry_at <= refused_at + crate::app_native::CAPTURE_REFUSAL_BASELINE_PROBE,
        "the lane looks again within the baseline probe"
    );
    drop(busy);
    app.auto_apply_intent = Some(crate::AutoApplyIntent {
        retry_at: std::time::Instant::now() - Duration::from_secs(1),
        ..intent
    });
    app.last_update_activity_at = std::time::Instant::now();
    app.try_pending_native_auto_apply(false);
    let record = app
        .auto_apply_capture_refusal
        .as_ref()
        .expect("the refusal is on record");
    assert!(
        !record.released,
        "PRECONDITION: the poll admitted no attempt"
    );
    assert!(
        record.desk.complete(),
        "the busy tab's first readable state is its baseline, read by the poll itself"
    );
}

#[test]
fn a_refusal_that_came_back_is_recorded_with_its_spacing() {
    let _ledger = crate::app_update_screen::hold_update_ledger_for_test();
    let mut app = App::headless_for_test();
    let build = stage_one_build(&mut app);
    let snapshot = app.native_updater_service.snapshot();
    let running = snapshot.current_build;
    let version = snapshot.staged.as_ref().expect("staged").version.clone();
    for refusal in 1..=3 {
        let before = app.messages.log().records().count();
        app.record_update_switch_started(running, false, true);
        let started = app.messages.log().records().count() - before;
        assert_eq!(
            started,
            usize::from(refusal == 1),
            "refusal {refusal}: the switch's start is on record once"
        );
        refuse_capture(&mut app, build);
        let added: Vec<_> = app
            .messages
            .log()
            .records()
            .skip(before + started)
            .cloned()
            .collect();
        assert_eq!(added.len(), 1, "refusal {refusal}: {added:?}");
        assert_eq!(
            added[0].title,
            crate::update_words::waiting_to_install(Some(Some(&version))),
            "refusal {refusal}: every refusal of the build wears the switch's title"
        );
    }
}

#[test]
fn a_producer_failure_retries_where_a_preparation_failure_converges() {
    let _ledger = crate::app_update_screen::hold_update_ledger_for_test();
    for (outcome, converges) in [
        (crate::UpdateHandoffOutcome::ProducerFailed, false),
        (crate::UpdateHandoffOutcome::PreparationFailed, true),
    ] {
        let mut app = App::headless_for_test();
        let build = stage_one_build(&mut app);
        for _ in 0..crate::app_native::STRUCTURAL_FAILURE_LIFETIME_ATTEMPTS {
            reduce_one_returned_failure(
                &mut app,
                ApplyMode::AutomaticPastGrace,
                build,
                &"ab".repeat(32),
                outcome,
                crate::ChildDeathEvidence::Unobserved,
            );
        }
        let latch = app
            .auto_apply_manual_only
            .expect("a physical failure latches for its schedule's spacing");
        assert_eq!(
            is_structural_resample(latch.retry_at),
            converges,
            "{outcome:?}: converged={converges} after {} failures: {:?}",
            crate::app_native::STRUCTURAL_FAILURE_LIFETIME_ATTEMPTS,
            latch.retry_at
        );
    }
}

#[cfg(unix)]
#[test]
fn a_fork_lane_park_miss_is_timing_and_buys_the_next_rung() {
    let mut app = App::headless_for_test();
    let target = 4_242_u64;
    let budget = |app: &App| {
        super::handoff_freeze_budget(
            ApplyMode::Automatic,
            app.fork_park_misses_of(target),
            super::FreezeSeed::Default,
        )
    };
    let mut last = budget(&app);
    for misses in 1..=3_u8 {
        let error = app.fork_park_missed(target, "a PTY reader missed".to_string());
        assert!(
            error.is_park_missed(),
            "a fork-lane miss is typed as timing, never `Failed` (physical)"
        );
        assert_eq!(app.fork_park_misses_of(target), misses);
        assert!(budget(&app) >= last, "miss {misses}: never a narrower rung");
        if misses == 1 {
            assert!(budget(&app) > last, "the first miss buys the next rung");
        }
        last = budget(&app);
    }
    assert_eq!(
        app.fork_park_misses_of(target + 1),
        0,
        "another build starts over"
    );
}

#[cfg(unix)]
#[test]
fn a_fork_lane_park_miss_is_recorded_as_the_routine_wait_it_is() {
    let mut app = App::headless_for_test();
    let miss = app.fork_park_missed(
        7,
        "a PTY reader missed the 20 ms handoff park deadline".to_string(),
    );
    for mode in [ApplyMode::Automatic, ApplyMode::AutomaticPastGrace] {
        assert!(miss.stops_routinely(mode), "{mode:?}: a timing miss");
    }
    let mode = ApplyMode::Immediate;
    assert!(!miss.stops_routinely(mode), "{mode:?}: a person asked");
    let activity = crate::UpdateHandoffStartError::activity("typing");
    assert!(activity.stops_routinely(ApplyMode::Immediate));
    let refusal = crate::UpdateHandoffStartError::capture_refused("session 3");
    assert!(refusal.stops_routinely(ApplyMode::Automatic));
    assert!(!refusal.stops_routinely(ApplyMode::Immediate));
    for other in [
        crate::UpdateHandoffStartError::failed("masters closed"),
        crate::UpdateHandoffStartError::refused("unowned socket"),
    ] {
        assert!(!other.stops_routinely(ApplyMode::Automatic), "{other}");
    }
}

#[test]
fn every_handoff_outcome_lands_in_the_lane_its_evidence_earns() {
    use crate::ChildDeathEvidence as Death;
    use crate::UpdateHandoffOutcome as Outcome;
    use crate::app_native::{HandoffFailureLane as Lane, PhysicalFailureShape as Shape};

    for (outcome, expected) in [
        (Outcome::AdoptionMismatch, Lane::Physical(Shape::Structural)),
        (
            Outcome::PreparationFailed,
            Lane::Physical(Shape::Structural),
        ),
        (Outcome::ProducerFailed, Lane::Physical(Shape::Transient)),
        (Outcome::CaptureRefused, Lane::Refused),
        (Outcome::ChildDied, Lane::Physical(Shape::Unexplained)),
        (Outcome::TimedOut, Lane::Physical(Shape::Transient)),
        (Outcome::Rejected, Lane::Physical(Shape::Transient)),
        (Outcome::ActivityRevoked, Lane::ActivityRevoked),
        (Outcome::ProofReady, Lane::Physical(Shape::Transient)),
    ] {
        assert_eq!(
            Lane::classify(
                ApplyMode::AutomaticPastGrace,
                outcome,
                Death::Unobserved,
                false
            ),
            expected,
            "{outcome:?} in the background lane"
        );
        assert_eq!(
            Lane::classify(ApplyMode::Immediate, outcome, Death::Unobserved, false),
            Lane::Manual,
            "{outcome:?} from a person"
        );
        assert_eq!(
            Lane::classify(
                ApplyMode::AutomaticPastGrace,
                outcome,
                Death::Unobserved,
                true
            ),
            if outcome == Outcome::CaptureRefused {
                Lane::Refused
            } else {
                Lane::ActivityRevoked
            },
            "{outcome:?} with a main-thread activity revocation"
        );
    }
}

#[test]
fn a_starved_child_died_keeps_retrying_past_the_budget_a_refusal_would_spend() {
    use crate::app_native::{
        PHYSICAL_FAILURE_LIFETIME_ATTEMPTS, STRUCTURAL_FAILURE_LIFETIME_ATTEMPTS,
    };
    let _ledger = crate::app_update_screen::hold_update_ledger_for_test();
    let mut app = App::headless_for_test();
    let build = stage_one_build(&mut app);
    let starved = crate::ChildDeathEvidence::Signalled {
        signal: libc::SIGKILL,
    };
    let deadline = |app: &App| {
        app.auto_apply_manual_only
            .expect("an automatic physical failure always latches manual-only")
            .retry_at
    };

    for _ in 0..STRUCTURAL_FAILURE_LIFETIME_ATTEMPTS {
        reduce_one_returned_failure(
            &mut app,
            ApplyMode::AutomaticPastGrace,
            build,
            &"ab".repeat(32),
            crate::UpdateHandoffOutcome::ChildDied,
            starved,
        );
    }
    let still_coming = deadline(&app)
        .expect(
            "THE REGRESSION: a machine that killed our candidate has said \
             nothing about the new bytes, so the automatic lane must still be \
             scheduled — `None` here is the converged latch the old \
             unconditional Structural classification minted after two of these",
        )
        .saturating_duration_since(std::time::Instant::now());
    assert!(
        still_coming > std::time::Duration::from_secs(1700)
            && still_coming <= std::time::Duration::from_secs(1800),
        "and it rides the epoch schedule's second rung (~1800s), which is what \
         proves it took the machine-shaped lane rather than a shortened one: \
         got {still_coming:?}"
    );

    for _ in STRUCTURAL_FAILURE_LIFETIME_ATTEMPTS..PHYSICAL_FAILURE_LIFETIME_ATTEMPTS {
        reduce_one_returned_failure(
            &mut app,
            ApplyMode::AutomaticPastGrace,
            build,
            &"ab".repeat(32),
            crate::UpdateHandoffOutcome::ChildDied,
            starved,
        );
    }
    let resample = deadline(&app)
        .expect("a spent transient budget re-samples after the cooldown")
        .saturating_duration_since(std::time::Instant::now());
    assert!(
        resample > std::time::Duration::from_secs(5 * 60 * 60 + 59 * 60)
            && resample <= std::time::Duration::from_secs(6 * 60 * 60),
        "the transient budget still ends: past it the lane waits a whole epoch \
         cooldown (6 h) per attempt, got {resample:?}"
    );
}

#[test]
fn a_refusing_child_died_converges_to_manual_only_within_its_budget() {
    use crate::app_native::STRUCTURAL_FAILURE_LIFETIME_ATTEMPTS;
    let _ledger = crate::app_update_screen::hold_update_ledger_for_test();
    let mut app = App::headless_for_test();
    let build = stage_one_build(&mut app);
    let refused = crate::ChildDeathEvidence::Exited { code: 0 };
    let deadline = |app: &App| {
        app.auto_apply_manual_only
            .expect("an automatic physical failure always latches manual-only")
            .retry_at
    };

    reduce_one_returned_failure(
        &mut app,
        ApplyMode::AutomaticPastGrace,
        build,
        &"ab".repeat(32),
        crate::UpdateHandoffOutcome::ChildDied,
        refused,
    );
    let confirming = deadline(&app)
        .expect("ONE unlucky handoff never converges a lane, whatever it said")
        .saturating_duration_since(std::time::Instant::now());
    assert!(
        confirming > std::time::Duration::from_secs(500)
            && confirming <= std::time::Duration::from_secs(600),
        "the confirming retry is the schedule's first rung (~600s), got \
         {confirming:?}"
    );

    for _ in 1..STRUCTURAL_FAILURE_LIFETIME_ATTEMPTS {
        reduce_one_returned_failure(
            &mut app,
            ApplyMode::AutomaticPastGrace,
            build,
            &"ab".repeat(32),
            crate::UpdateHandoffOutcome::ChildDied,
            refused,
        );
    }
    assert!(
        is_structural_resample(deadline(&app)),
        "twice told that this successor will not become our successor is not a \
         busy afternoon: the lane's schedule is done with these bytes, and only \
         the verdict's ONE re-sample, a day out, is left: {:?}",
        deadline(&app)
    );
    assert!(
        app.auto_apply_structural_verdict
            .is_some_and(|verdict| verdict.build == build),
        "the convergence is recorded as a structural verdict"
    );
}

#[test]
fn a_successor_that_exits_75_is_retried_on_the_transient_schedule() {
    use crate::app_native::STRUCTURAL_FAILURE_LIFETIME_ATTEMPTS;
    let _ledger = crate::app_update_screen::hold_update_ledger_for_test();
    let mut app = App::headless_for_test();
    let build = stage_one_build(&mut app);
    let deferred = crate::ChildDeathEvidence::Exited {
        code: crate::seamless::EXIT_SUCCESSOR_PASSING,
    };
    for _ in 0..STRUCTURAL_FAILURE_LIFETIME_ATTEMPTS {
        reduce_one_returned_failure(
            &mut app,
            ApplyMode::AutomaticPastGrace,
            build,
            &"ab".repeat(32),
            crate::UpdateHandoffOutcome::ChildDied,
            deferred,
        );
    }
    let wait = app
        .auto_apply_manual_only
        .expect("an automatic physical failure always latches")
        .retry_at
        .expect("a moment always has a deadline")
        .saturating_duration_since(std::time::Instant::now());
    assert!(
        wait > std::time::Duration::from_secs(1700) && wait <= std::time::Duration::from_secs(1800),
        "the structural lane's whole lifetime later, a deferred swap is on the \
         transient schedule's second rung (~1800 s), got {wait:?}"
    );
    assert!(
        app.auto_apply_structural_verdict.is_none(),
        "no structural verdict is minted from a moment"
    );
    assert!(
        super::forgives_the_counted_trial_launch(deferred, build - 1, build),
        "the new image never ran, so its counted trial launch is given back"
    );
}

#[test]
fn a_pre_park_check_that_did_not_finish_is_filed_transient() {
    use crate::app_native::{HandoffFailureLane, PhysicalFailureShape};
    let passing = format!(
        "bundle policy: codesign --verify (team-pinned) ran past this apply's \
         verification budget; treating as a rejection ({})",
        aterm_update::PASSING_REFUSAL_KEY
    );
    let lock = format!(
        "pre-verify lock: another process has held the update lock for more than 10s ({})",
        aterm_update::PASSING_REFUSAL_KEY
    );
    for error in [passing.as_str(), lock.as_str()] {
        let outcome = super::pre_park_refusal_outcome(error);
        assert_eq!(
            outcome,
            crate::UpdateHandoffOutcome::ProducerFailed,
            "{error}"
        );
        assert_eq!(
            HandoffFailureLane::classify(
                ApplyMode::AutomaticPastGrace,
                outcome,
                crate::ChildDeathEvidence::Unobserved,
                false,
            ),
            HandoffFailureLane::Physical(PhysicalFailureShape::Transient),
            "{error}"
        );
    }
    let verdict = "bundle policy: codesign --verify (team-pinned requirement) failed: code \
                   object is not signed at all";
    assert_eq!(
        super::pre_park_refusal_outcome(verdict),
        crate::UpdateHandoffOutcome::PreparationFailed
    );
    assert_eq!(
        HandoffFailureLane::classify(
            ApplyMode::AutomaticPastGrace,
            super::pre_park_refusal_outcome(verdict),
            crate::ChildDeathEvidence::Unobserved,
            false,
        ),
        HandoffFailureLane::Physical(PhysicalFailureShape::Structural)
    );
}

#[test]
fn an_unexplained_child_died_gets_a_second_epoch_and_then_only_resamples() {
    use crate::app_native::{
        PHYSICAL_FAILURE_LIFETIME_ATTEMPTS, PHYSICAL_FAILURES_PER_EPOCH,
        UNEXPLAINED_FAILURE_LIFETIME_ATTEMPTS,
    };
    let _ledger = crate::app_update_screen::hold_update_ledger_for_test();
    let mut app = App::headless_for_test();
    let build = stage_one_build(&mut app);
    let deadline = |app: &App| {
        app.auto_apply_manual_only
            .expect("an automatic physical failure always latches manual-only")
            .retry_at
    };
    let spend = |app: &mut App, times: u8| {
        for _ in 0..times {
            reduce_one_returned_failure(
                app,
                ApplyMode::AutomaticPastGrace,
                build,
                &"ab".repeat(32),
                crate::UpdateHandoffOutcome::ChildDied,
                crate::ChildDeathEvidence::Unobserved,
            );
        }
    };

    spend(&mut app, PHYSICAL_FAILURES_PER_EPOCH);
    let stand_down = deadline(&app)
        .expect("the first epoch ends in a stand-down, not in convergence")
        .saturating_duration_since(std::time::Instant::now());
    assert!(
        stand_down > std::time::Duration::from_secs(5 * 60 * 60),
        "and the stand-down is hours, because re-sampling the SAME loaded hour \
         is what the in-epoch rungs already did: got {stand_down:?}"
    );

    spend(
        &mut app,
        UNEXPLAINED_FAILURE_LIFETIME_ATTEMPTS - PHYSICAL_FAILURES_PER_EPOCH,
    );
    let resample = deadline(&app)
        .expect("a spent unexplained budget re-samples after the cooldown")
        .saturating_duration_since(std::time::Instant::now());
    assert!(
        resample > std::time::Duration::from_secs(5 * 60 * 60 + 59 * 60)
            && resample <= std::time::Duration::from_secs(6 * 60 * 60),
        "one epoch cooldown out: got {resample:?} after \
         {UNEXPLAINED_FAILURE_LIFETIME_ATTEMPTS} of {PHYSICAL_FAILURE_LIFETIME_ATTEMPTS}"
    );
}

#[test]
fn a_dead_candidate_is_classified_by_what_the_worker_saw() {
    use crate::ChildDeathEvidence as Death;
    use crate::UpdateHandoffOutcome as Outcome;
    use crate::app_native::{HandoffFailureLane as Lane, PhysicalFailureShape as Shape};

    for (death, expected, why) in [
        (
            Death::Unobserved,
            Shape::Unexplained,
            "no witnessed status, or one that could only have been our own \
             SIGKILL — and a verdict nobody witnessed must not be filed as \
             one somebody did",
        ),
        (
            Death::Exited { code: 0 },
            Shape::Structural,
            "a clean exit is this tree's COMMONEST refusal — `main_entry` \
             returns without a window when the overlap authority is \
             incomplete — and reaching an exit instruction at all is proof \
             the image ran",
        ),
        (
            Death::Exited { code: 74 },
            Shape::Structural,
            "the fail-stop exit a candidate takes when it can never become \
             authoritative; same reading, which is why the CODE is recorded \
             and not judged",
        ),
        (
            Death::Signalled {
                signal: libc::SIGSEGV,
            },
            Shape::Structural,
            "a fault is the image executing itself into a wall, which is the \
             bytes",
        ),
        (
            Death::Signalled {
                signal: libc::SIGABRT,
            },
            Shape::Structural,
            "so is an abort",
        ),
        (
            Death::Signalled {
                signal: libc::SIGKILL,
            },
            Shape::Transient,
            "THE FIELD CASE: no process raises SIGKILL on itself, and macOS \
             jetsam sends it to reclaim memory. The next attempt on a calmer \
             machine is the one that wins",
        ),
        (
            Death::Signalled {
                signal: libc::SIGTERM,
            },
            Shape::Transient,
            "and any other signal from outside the image is equally not a \
             statement about the bytes",
        ),
    ] {
        assert_eq!(
            Lane::classify(
                ApplyMode::AutomaticPastGrace,
                Outcome::ChildDied,
                death,
                false
            ),
            Lane::Physical(expected),
            "{death:?}: {why}"
        );
        assert_eq!(
            Lane::classify(
                ApplyMode::AutomaticPastGrace,
                Outcome::TimedOut,
                death,
                false
            ),
            Lane::Physical(Shape::Transient),
            "{death:?} must not move a TimedOut, whose candidate we killed"
        );
    }
}
