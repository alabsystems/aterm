// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use std::time::Duration;

use crate::App;
use crate::native_updater_service::{
    ApplyAttemptTicket, ApplyMode, CheckCompletion, CheckStart, DurableUpdateStatus,
};

const TEST_COMMIT: &str = "0123456789abcdef0123456789abcdef01234567";

/// Stage one strictly-newer build through the real check reducer, so the
/// artifact the completion reduces below is the one production would hold.
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
                installable: true,
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
/// Named by its schedule, not to the second, since these are real `Instant`s.
fn is_structural_resample(retry_at: Option<std::time::Instant>) -> bool {
    retry_at.is_some_and(|at| {
        let wait = at.saturating_duration_since(std::time::Instant::now());
        wait > crate::app_native::STRUCTURAL_RESAMPLE_AFTER - std::time::Duration::from_secs(60)
            && wait <= crate::app_native::STRUCTURAL_RESAMPLE_AFTER
    })
}

/// Drives one returned handoff for `mode` end to end through the production
/// reducer: an authorized attempt is pending, the worker reports `outcome`
/// against the artifact `digest` names, and the reducer decides the cost.
///
/// Nothing is hand-built: the ticket is the reducer's current apply, the pending
/// record carries the authorized mode, and the completion has `reconcile: None`
/// as every construction site emits. The outcome and the death evidence are
/// parameters because both select the budget.
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

/// A physical failure a person asked for (Version menu, palette,
/// `aterm-ctl update apply`, install on clean quit) is not charged to the
/// automatic lane's budget. Both lanes are driven against the same artifact in
/// the same `App` with identical worker reports; the contrast is the assertion.
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

    // A person's failure charges nothing: not the physical budget, the
    // manual-only latch or the live automatic intent.
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

    // The same failure authorized in the background is charged in full; ignoring
    // the mode would make the block above produce this state.
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

/// The activity classification stays on the background lane: a person's attempt
/// arms no revocation watcher, so an activity-shaped rejection against it must
/// not mint an `arm_activity_revoked_overlap_retry` cycle.
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

/// Round eight of the update audit, lead U39 (r8d #738): an AUTOMATIC attempt the
/// lane tries again by itself — a capture refusal (retried when the desk
/// changes), a physical failure inside its budget (retried in ten minutes) — was
/// put on record as "⚠ Couldn't install aterm vX", the needs-you glyph and a
/// Problem in Settings ▸ Messages. The stop is said once the lane has decided: a
/// lane that tries again by itself waits, in Info, and says so; one that
/// stopped, or a person's press, could not install. A capture refusal's stop
/// is its own outcome's one record, titled by the switch.
#[test]
fn a_switch_the_automatic_lane_retries_by_itself_is_recorded_as_waiting() {
    let _ledger = crate::app_update_screen::hold_update_ledger_for_test();
    let stops = |app: &App| {
        app.messages
            .log()
            .records()
            // The switch's own stop: "aterm vY kept running …", or the
            // capture refusal's record that says it.
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
        // NEGATIVE CONTROL: a person's press that failed is not retried.
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

/// The typed failure kind reaches the budget: "missed a 15 s deadline" and
/// "these two images cannot agree on an adoption proof" cost differently.
/// Driven through `reduce_returned_handoff_completion` with two artifacts in one
/// `App`, so each has its own budget and only the worker's verdict differs.
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

    // AdoptionMismatch twice: a confirmation, then the end of the schedule for
    // those bytes, converged onto the one re-sample a day out (`arm` reads it as
    // `SuppressManualOnly` until due).
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

    // TimedOut twice on different bytes: still scheduled, on the second rung,
    // proving the two arcs have separate counters.
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

/// Through the production reducer: an automatic attempt whose park missed every
/// rung returns with `park_miss_disposition`'s outcome and is treated as
/// activity: a live retry intent on the spaced schedule, no manual-only latch, no
/// physical budget spent, the ladder's anchor unchanged. Twice.
#[test]
fn a_park_that_missed_every_rung_is_retried_as_activity_and_never_latches() {
    let _ledger = crate::app_update_screen::hold_update_ledger_for_test();
    let mut app = App::headless_for_test();
    let build = stage_one_build(&mut app);
    // Inside KeysOnly, where a busy machine is admitted.
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

/// A capture refusal is its own lane, through the production reducer: recorded as
/// an apply failure (`failing_applies` moves, `update status` names it), retried
/// only once the desk changes or the hourly backstop passes, never on the
/// activity spacing, never charged to the physical budget, never latched. Twice.
// The bundle apply lane and its ledger (`aterm_update::apply_lane_report`) are
// macOS's: a Linux update replaces the executable on disk and hands nothing off.
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
    // The scratch ledger is shared with sibling tests (under the lock held above),
    // so counts are compared step to step.
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

    // An unchanged desk holds the next attempt: no successor is launched into
    // the same refusal.
    let now = std::time::Instant::now();
    assert!(app.capture_refusal_holds(build, dmg, false, now));
    assert!(app.capture_refusal_holds(build, dmg, false, now));
    // …a DIFFERENT artifact is not held by it…
    assert!(!app.capture_refusal_holds(build + 1, dmg, false, now));
    // …and the desk moving releases it (a resize is one of the facts every
    // refusal the capture can make is about), once the spacing of a second
    // refusal in a row has passed (`capture_refusal_spacing`).
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

    // The backstop: an hour with the desk unchanged tries once more anyway.
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

/// Arm the refusal lane for `build` through the production reducer, as a
/// returned `CaptureRefused` completion does.
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

/// A refused desk is not released by WHERE A READ ENDED (round eight of the
/// update audit, U43). The fingerprint hashed each parser's ground state, which
/// flips with every PTY read that ends inside an escape sequence: a streaming
/// tab released the refusal at the next 30 s probe, and each release was a full
/// launch, freeze and export into the same deterministic refusal. No capture
/// refusal is about it (a parser mid-sequence is a park MISS, never this lane),
/// so it is no desk fact. Negative control: a resize still releases.
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

/// A REFUSAL THAT COMES BACK IS SPACED (round eight of the update audit, U43).
/// The fingerprint is a guess at what the refusal was about: a busy desk moves
/// it (a `cd`, a TUI on the alternate screen, a pane opened) at nearly every
/// probe whatever refused, and 43c7d0936 put `ManifestOverCap` on this lane,
/// where it was re-attempted every 30 s with no latch — each attempt a launch,
/// a freeze of every tab and a history export, against the 600 s and 1800 s
/// it had as a producer failure. A desk change releases the first refusal at
/// the probe, as before (the update lands soon after the offending tab
/// closes); a second refusal in a row holds 600 s, a third and later 1800 s,
/// under the hourly backstop. Never a latch: each one still releases.
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
        refuse_capture(&mut app, build);
        let refused_at = std::time::Instant::now();
        move_desk(&mut app);
        assert!(
            app.capture_refusal_holds(
                build,
                dmg,
                false,
                refused_at + Duration::from_secs(floor - 2)
            ),
            "refusal {refusal}: a changed desk is not attempted before {floor} s"
        );
        assert!(
            !app.capture_refusal_holds(
                build,
                dmg,
                false,
                refused_at + Duration::from_secs(floor + 2)
            ),
            "refusal {refusal}: a changed desk is attempted at {floor} s"
        );
        assert!(
            app.auto_apply_manual_only.is_none(),
            "refusal {refusal}: never a latch"
        );
    }
    // The streak is about one update: another build's first refusal is
    // released at the probe.
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

/// A REFUSAL RECORDED ON A BUSY DESK TAKES ITS BASELINE AT THE PROBE (round
/// eight of the update audit, U43 review). A refusal comes back while the
/// readers drain what queued up during the freeze, so one tab's engine can be
/// locked when it is recorded and its desk is unreadable. Its baseline is the
/// first readable desk; when that read waited for the spacing (600 s for a
/// second refusal), a tab closed during the spacing became part of the
/// baseline, nothing differed from it, and the attempt waited for the hourly
/// backstop. The lane now comes back at the probe for the baseline, and the
/// spacing holds back only the release.
#[test]
fn a_refusal_recorded_on_a_busy_desk_takes_its_baseline_at_the_probe() {
    let _ledger = crate::app_update_screen::hold_update_ledger_for_test();
    let mut app = App::headless_for_test();
    let build = stage_one_build(&mut app);
    let dmg = [0xab_u8; 32];
    refuse_capture(&mut app, build);
    // Refusal 2 is recorded while a reader holds its tab's engine. The arm
    // is called directly: the rest of the reducer's rollback waits for
    // that engine, which in production its reader lets go of.
    let busy = HeldEngine::hold(&app.pool.iter().next().expect("a session").term);
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
    // The probe takes the baseline, and still holds.
    assert!(app.capture_refusal_holds(build, dmg, false, refused_at + probe));
    assert!(
        app.auto_apply_capture_refusal
            .as_ref()
            .is_some_and(|refusal| refusal.desk.complete()),
        "the first readable desk is the baseline, inside the spacing"
    );
    // …after which nothing comes back before the spacing ends (the record's
    // clock was read a moment before `refused_at`).
    assert!(
        app.capture_refusal_reprobe_at(refused_at + probe)
            >= refused_at + spacing - Duration::from_secs(1)
    );
    // The person closes the offending tab at +60 s (a resize stands for it).
    for session in app.pool.iter() {
        crate::term_lock(&session.term).resize(31, 100);
    }
    assert!(
        app.capture_refusal_holds(
            build,
            dmg,
            false,
            refused_at + spacing - Duration::from_secs(2)
        ),
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

/// A reader holding one session's engine, as a reader draining a flood
/// does, until dropped.
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
            // A hang detector, never a latency budget: dropping the guard
            // disconnects this at once.
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

/// A desk of three tabs whose first one a reader holds, with `build`'s
/// second capture refusal in a row recorded on it (spaced 600 s): its
/// baseline knows all three tabs and lacks only the busy one's reading.
/// Returns the build, the held engine, the two quiet tabs' ids and when it
/// was refused.
fn second_refusal_beside_a_busy_engine(
    app: &mut App,
) -> (u64, HeldEngine, [u64; 2], std::time::Instant) {
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
    (build, busy, quiet, refused_at)
}

/// A TAB CLOSED OR RESIZED BEFORE THE FIRST PROBE COUNTS, though another
/// tab's engine is busy at every read (round eight of the update audit, U43
/// second review). The desk fingerprint was one hash, unreadable whole while
/// any engine was locked: on a desk with a tab streaming output, a refusal's
/// baseline waited for the first probe that found every engine free, a tab
/// the person closed at once (as the record asks) was already part of it,
/// and the attempt waited for the hourly backstop instead of the 10 minutes
/// the record said. Which tabs are open is now read with no engine lock, and
/// a busy engine costs only its own tab's reading. Negative control: with
/// nothing changed, the busy tab says nothing about itself while busy, its
/// first reading after the spacing is its baseline, never a change, and a
/// later change of it releases.
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
        let (build, busy, quiet, refused_at) = second_refusal_beside_a_busy_engine(&mut app);
        // At once, before any probe, while the busy tab stays busy.
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
                app.capture_refusal_holds(build, dmg, false, refused_at + probe),
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

/// THE POLL READS A BUSY TAB'S BASELINE WHATEVER IT DECIDES (round eight of
/// the update audit, U43 second review). The hold reads the sessions a
/// refusal's baseline lacks, but a poll in the ladder's first phase that
/// waits for activity to settle never reaches the hold, and a tab first read
/// once the activity ends would absorb what the person did to it meanwhile.
/// The lane comes back within the baseline probe, and that look reads it.
#[test]
fn a_poll_that_waits_for_activity_still_reads_a_busy_tabs_baseline() {
    let _ledger = crate::app_update_screen::hold_update_ledger_for_test();
    let mut app = App::headless_for_test();
    let (_build, busy, _quiet, refused_at) = second_refusal_beside_a_busy_engine(&mut app);
    let intent = app.auto_apply_intent.expect("the lane keeps its intent");
    assert!(
        intent.retry_at <= refused_at + crate::app_native::CAPTURE_REFUSAL_BASELINE_PROBE,
        "the lane looks again within the baseline probe"
    );
    drop(busy);
    // That look comes while the person is busy, in the ladder's first phase
    // (the refusal anchored it now), so the poll waits for activity.
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

/// A REFUSAL THAT CAME BACK SAYS WHEN THE NEXT TRY IS, through the production
/// reducer. The one record a refusal leaves, titled by the switch it stopped
/// (`Waiting to install aterm vX`), is the visible notice for the lane's wait: from
/// the second refusal of an update it says the spacing, which no change of the
/// tabs shortens, and the first says nothing more, as before. Every refusal of
/// the build wears the same title: a later attempt opens no switch of its own
/// on record (one fact, one entry), and its record was once "Update waiting".
#[test]
fn a_refusal_that_came_back_is_recorded_with_its_spacing() {
    let _ledger = crate::app_update_screen::hold_update_ledger_for_test();
    let mut app = App::headless_for_test();
    let build = stage_one_build(&mut app);
    let snapshot = app.native_updater_service.snapshot();
    let running = snapshot.current_build;
    let version = snapshot.staged.as_ref().expect("staged").version.clone();
    for refusal in 1..=3 {
        // As the lane does: the switch is on record before the park refuses
        // (the first attempt's start; a later attempt at the build adds none).
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
        // ONE RECORD PER REFUSAL: no separate "kept running" stop beside it.
        assert_eq!(added.len(), 1, "refusal {refusal}: {added:?}");
        assert_eq!(
            added[0].title,
            crate::update_words::waiting_to_install(Some(Some(&version))),
            "refusal {refusal}: every refusal of the build wears the switch's title"
        );
    }
}

/// This process's own failures are not the candidate's, through the production
/// reducer: two `ProducerFailed` completions (a full disk at the manifest write,
/// `EMFILE` at the descriptor duplicate) stay on the transient schedule, while the
/// same two as `PreparationFailed` (the control) converge onto a structural
/// verdict's one re-sample a day out.
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

/// The fork lane climbs the freeze ladder across attempts: a miss rides the
/// activity spacing, and the rung is counted per target build: 20 ms, then
/// 80 ms, then a quarter second; a different build starts over.
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

/// The fork lane's automatic park miss is recorded like the launched lane's
/// activity stand-down (`HandoffFailureLane::ActivityRevoked`, routine), so the
/// switch record says "Waiting to install", while a person's apply (`Manual`)
/// stays a stop worth a warning.
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
    // The automatic lane's capture refusal arms the desk-change retry: a wait
    // too (round eight, lead U39); a person's press it refused is not.
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

/// The classification as a table, so a new outcome variant must be placed
/// deliberately. Three axes: who asked (the mode), what happened (the typed
/// outcome), and for `ChildDied`, what the worker saw.
///
/// Driven with `Unobserved` evidence, the default for every outcome but
/// `ChildDied`, which therefore lands in `Unexplained` here; its evidence-bearing
/// arms are the table below.
#[test]
fn every_handoff_outcome_lands_in_the_lane_its_evidence_earns() {
    use crate::ChildDeathEvidence as Death;
    use crate::UpdateHandoffOutcome as Outcome;
    use crate::app_native::{HandoffFailureLane as Lane, PhysicalFailureShape as Shape};

    for (outcome, expected) in [
        (Outcome::AdoptionMismatch, Lane::Physical(Shape::Structural)),
        // Only the candidate failing its pre-park verification.
        (
            Outcome::PreparationFailed,
            Lane::Physical(Shape::Structural),
        ),
        // This process's own write/descriptor failures: about the moment and
        // the machine, never the bytes.
        (Outcome::ProducerFailed, Lane::Physical(Shape::Transient)),
        // A deterministic capture refusal: its own lane, never a physical
        // shape and never activity.
        (Outcome::CaptureRefused, Lane::Refused),
        // Neither `Structural` nor `Transient`: with nothing observed, proof
        // EOF is a failure whose kind is unknown.
        (Outcome::ChildDied, Lane::Physical(Shape::Unexplained)),
        (Outcome::TimedOut, Lane::Physical(Shape::Transient)),
        (Outcome::Rejected, Lane::Physical(Shape::Transient)),
        (Outcome::ActivityRevoked, Lane::ActivityRevoked),
        // Unreachable as a failure (the commit path handles it); fails closed to
        // the forgiving shape.
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
        // A person's apply charges nothing whatever the worker reported.
        assert_eq!(
            Lane::classify(ApplyMode::Immediate, outcome, Death::Unobserved, false),
            Lane::Manual,
            "{outcome:?} from a person"
        );
        // The main thread's own activity-shaped rejection dominates the physical
        // shape (a rollback the terminal caused says nothing about the artifact),
        // but not a capture refusal, the park's own typed verdict about the desk
        // (`RefusalNeverRetriesAsActivity`).
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

/// A starved child retries. Under heavy load a successor can end before its
/// proof (`ChildDied`) while the same build applies cleanly once the load stops;
/// charging that the structural budget would leave the user on "staged, applies
/// on relaunch" for a merely busy machine.
///
/// `SIGKILL` is what that looks like when the parent can see it (macOS jetsam),
/// so this drives the structural budget's worth of them and asserts the lane
/// still comes back, then spends the rest, since retrying forever would be the
/// opposite defect.
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

    // Still bounded: spend the rest of the transient lifetime and the schedule
    // ends, said once. What follows is a quiet re-sample one epoch cooldown out,
    // never a latch that never lapses: at most one attempt per six hours.
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

/// A successor that refuses converges on the structural budget, but not on the
/// first failure. `Exited { code: 0 }` is the refusal this tree produces: a
/// candidate whose boot apply could not swap stays the old build, refuses the
/// target identity, closes every adopted master and returns from `main_entry`.
/// Reaching an `exit` separates it from a starved child.
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

/// A successor whose swap was deferred for a moment is retried, not converged: its
/// boot apply waited out a sibling's apply lock (or `codesign` ran out of budget),
/// stayed the old build and exited [`crate::seamless::EXIT_SUCCESSOR_PASSING`].
/// The two failures that converge an exit `0` ride the transient schedule here:
/// second rung, no structural verdict, trial launch forgiven.
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

/// A pre-park check that did not finish is this process's trouble: the worker
/// files it `ProducerFailed`, which the completion classifies Transient, while a
/// verdict stays `PreparationFailed` (Structural).
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

/// A death nobody witnessed ([`crate::ChildDeathEvidence::Unobserved`]), the
/// commonest `ChildDied` answer on the macOS lane: a refusing successor keeps
/// running after closing the readiness channel, so the status reaped afterwards
/// is this process's own SIGKILL.
///
/// That verdict is weaker than both above: it converges, but only after an
/// independent sample of the machine, the stand-down at the end of the first
/// epoch, rather than at the 600 s and 1800 s rungs that re-run the same bad
/// hour.
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
    // Two independent samples is where "a bad hour" stops justifying spending
    // the schedule: {UNEXPLAINED_FAILURE_LIFETIME_ATTEMPTS} attempts, fewer than
    // the {PHYSICAL_FAILURE_LIFETIME_ATTEMPTS} a named transient failure gets
    // (pinned by a compile-time assert). The verdict is still unexplained, so a
    // quiet re-sample one epoch cooldown out remains.
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

/// The evidence axis as a total table: every `ChildDeathEvidence` a worker can
/// produce and the shape it earns. A child starved of CPU also exits before its
/// readiness proof, so `ChildDied` cannot be structural unconditionally. Every
/// arm is a fact the worker observed, never a duration threshold or a message.
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
        // The evidence is read for `ChildDied` only; every other outcome is a
        // candidate this process ended, whose status is our SIGKILL.
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
