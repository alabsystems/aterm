// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use super::ColdRelaunch;
use crate::native_updater_service::{ApplyAttemptTicket, ApplyMode};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::Instant;

const COMMIT: &str = "0123456789abcdef0123456789abcdef01234567";

fn artifact() -> String {
    "ab".repeat(32)
}

/// The boot swap's step-5 refusal, spelled as `install_location_refusal` spells it.
fn install_location_refusal() -> String {
    format!(
        "{}: /Applications/aterm.app; this install cannot update itself",
        aterm_update::INSTALL_LOCATION_REFUSAL_KEY
    )
}

fn verdict(app: &crate::App, build: u64, passed: bool, reason: Option<String>) {
    *app.handoff_preverified
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner) = Some(crate::HandoffPreverification {
        build,
        commit: COMMIT.to_string(),
        artifact: artifact(),
        at: Instant::now(),
        passed,
        reason,
        policy: None,
        grant_chunks: false,
    });
}

#[test]
fn the_cold_lane_honours_a_cached_refusal_before_it_relaunches() {
    let mut app = crate::App::headless_for_test();
    let target = crate::running_build_number().saturating_add(1);
    let ticket = ApplyAttemptTicket::for_test(target, COMMIT, &artifact());
    verdict(&app, target, false, Some(install_location_refusal()));

    for mode in [ApplyMode::Automatic, ApplyMode::AutomaticPastGrace] {
        let error = app
            .cold_lane_verdict_gate(mode, Some(&ticket))
            .expect_err("the verdict answers the automatic lane");
        let message = error.to_string();
        assert!(
            !error.is_refused() && !error.is_activity_deferred(),
            "the seamless lane's shape — a Failed the synchronous arm books by the \
             cached verdict: {error:?}"
        );
        assert!(
            message.contains(aterm_update::INSTALL_LOCATION_REFUSAL_KEY),
            "the verifier's own words: {message}"
        );
        #[cfg(target_os = "macos")]
        assert!(
            crate::update_apply_trouble::ApplyTrouble::needs_person(&message),
            "so the arm latches the environment block, with no retry: {message}"
        );
    }

    // NEGATIVE CONTROLS: a person's apply is not answered by the cache (the
    // seamless lane's rule), the QA seam has no artifact, and a pass relaunches.
    assert!(
        app.cold_lane_verdict_gate(ApplyMode::Immediate, Some(&ticket))
            .is_ok()
    );
    assert!(
        app.cold_lane_verdict_gate(ApplyMode::Automatic, None)
            .is_ok()
    );
    verdict(&app, target, true, None);
    assert!(
        app.cold_lane_verdict_gate(ApplyMode::Automatic, Some(&ticket))
            .is_ok()
    );
}

/// No verdict yet: the arm-time read the first attempt outran is waited for —
/// as activity, bounded — and the attempt after it relaunches under its answer.
#[test]
fn the_cold_lane_waits_for_a_verdict_being_read_and_no_longer() {
    let mut app = crate::App::headless_for_test();
    let target = crate::running_build_number().saturating_add(1);
    let ticket = ApplyAttemptTicket::for_test(target, COMMIT, &artifact());
    let done = Arc::new(AtomicBool::new(false));
    app.handoff_preverify_in_flight = Some(crate::PreverifyInFlight {
        build: target,
        commit: COMMIT.to_string(),
        artifact: artifact(),
        deadline: Instant::now() + crate::HANDOFF_PREVERIFY_READ_CEILING,
        done: Arc::clone(&done),
        passing: Default::default(),
    });
    for mode in [ApplyMode::Automatic, ApplyMode::AutomaticPastGrace] {
        let wait = app
            .cold_lane_verdict_gate(mode, Some(&ticket))
            .expect_err("a read of this target is running");
        assert!(wait.stops_routinely(mode), "routine activity: {wait}");
    }
    // The read ends with a refusal on record: the next attempt is answered by it.
    verdict(&app, target, false, Some(install_location_refusal()));
    done.store(true, Ordering::Release);
    assert!(
        app.cold_lane_verdict_gate(ApplyMode::Automatic, Some(&ticket))
            .is_err_and(|error| !error.is_activity_deferred())
    );
    // …and a read that ended with NOTHING on record (a check that did not
    // finish) is not waited for: the lane relaunches as it always did.
    let other = target.saturating_add(1);
    let ended = ApplyAttemptTicket::for_test(other, COMMIT, &artifact());
    app.handoff_preverify_in_flight = Some(crate::PreverifyInFlight {
        build: other,
        commit: COMMIT.to_string(),
        artifact: artifact(),
        deadline: Instant::now() + crate::HANDOFF_PREVERIFY_READ_CEILING,
        done: Arc::new(AtomicBool::new(true)),
        passing: Default::default(),
    });
    assert!(
        app.cold_lane_verdict_gate(ApplyMode::Automatic, Some(&ended))
            .is_ok()
    );
}

/// Round eight, S1's second review (P3): a read that ENDED WITHOUT A VERDICT —
/// the apply lock a stopped sibling holds, past its wait — is never cached as a
/// verdict, so the cold lane saw nothing, relaunched, and its successor (this
/// build) waited on the same lock in its boot swap and refused: the window
/// closed and reopened at every retry of the transient schedule, and every six
/// hours for good once it converged. Now that read's moment answers the
/// attempt: a `Failed` with the passing reason, which the synchronous arm books
/// TRANSIENT (no cached refusal stands behind it, and it is no person's to
/// clear), and nothing is relaunched. A later attempt's read of the same target
/// is waited for even once the per-target window is spent, and it is ITS answer
/// that counts.
///
/// RED before: the gate returned `Ok` on the ended read, and the cold arm went
/// on to write the layout and `exec`.
#[test]
fn a_read_that_ended_without_a_verdict_answers_the_cold_lane() {
    let mut app = crate::App::headless_for_test();
    let target = crate::running_build_number().saturating_add(1);
    let ticket = ApplyAttemptTicket::for_test(target, COMMIT, &artifact());
    let passing_reason = format!(
        "staged update failed pre-park verification: lock: could not take the apply \
         lock within 10s (another aterm holds it) ({})",
        aterm_update::PASSING_REFUSAL_KEY
    );
    let ended_passing = |reason: &str, ended: Instant| {
        let passing = Arc::new(std::sync::OnceLock::new());
        passing
            .set(crate::PassingPreverification {
                reason: reason.to_string(),
                ended,
            })
            .unwrap();
        crate::PreverifyInFlight {
            build: target,
            commit: COMMIT.to_string(),
            artifact: artifact(),
            deadline: Instant::now() + crate::HANDOFF_PREVERIFY_READ_CEILING,
            done: Arc::new(AtomicBool::new(true)),
            passing,
        }
    };
    app.handoff_preverify_in_flight = Some(ended_passing(&passing_reason, Instant::now()));
    // The window this target's first wait opened is spent.
    app.preverify_read_waited = Some((target, Instant::now()));

    for mode in [ApplyMode::Automatic, ApplyMode::AutomaticPastGrace] {
        let error = app
            .cold_lane_verdict_gate(mode, Some(&ticket))
            .expect_err("the moment the read measured answers the attempt");
        let message = error.to_string();
        assert!(
            !error.is_refused() && !error.is_activity_deferred(),
            "a Failed the synchronous arm books: {error:?}"
        );
        assert!(
            aterm_update::is_passing_refusal(&message) && message.contains("not relaunched"),
            "the read's own words, a passing moment: {message}"
        );
        assert!(
            !crate::update_apply_trouble::ApplyTrouble::needs_person(&message),
            "no person's to clear: {message}"
        );
    }
    // What the synchronous arm books it by: no cached refusal, so TRANSIENT.
    assert_eq!(app.cached_handoff_preverification(&ticket), None);
    assert_eq!(app.cached_handoff_refusal_reason(target), None);
    assert_eq!(
        crate::app_native::PhysicalFailureShape::cached_refusal_shape(None),
        crate::app_native::PhysicalFailureShape::Transient
    );

    // The next attempt's arm-time read, still running: waited for — as
    // activity — although the window is spent.
    let done = Arc::new(AtomicBool::new(false));
    app.handoff_preverify_in_flight = Some(crate::PreverifyInFlight {
        done: Arc::clone(&done),
        passing: Default::default(),
        ..ended_passing(&passing_reason, Instant::now())
    });
    let wait = app
        .cold_lane_verdict_gate(ApplyMode::Automatic, Some(&ticket))
        .expect_err("this target's read is running");
    assert!(wait.is_activity_deferred(), "{wait:?}");
    // It ends with a pass: the lane relaunches.
    verdict(&app, target, true, None);
    done.store(true, Ordering::Release);
    assert!(
        app.cold_lane_verdict_gate(ApplyMode::Automatic, Some(&ticket))
            .is_ok()
    );

    // NEGATIVE CONTROLS: a passing moment older than a verdict's freshness, and
    // another artifact's, answer nothing; a person's apply is not answered.
    *app.handoff_preverified
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner) = None;
    let stale = Instant::now()
        .checked_sub(crate::HANDOFF_PREVERIFY_FRESHNESS)
        .expect("the clock is past ten minutes of uptime");
    app.handoff_preverify_in_flight = Some(ended_passing(&passing_reason, stale));
    assert!(
        app.cold_lane_verdict_gate(ApplyMode::Automatic, Some(&ticket))
            .is_ok()
    );
    app.handoff_preverify_in_flight = Some(ended_passing(&passing_reason, Instant::now()));
    let other = ApplyAttemptTicket::for_test(target, COMMIT, &"cd".repeat(32));
    assert!(
        app.cold_lane_verdict_gate(ApplyMode::Automatic, Some(&other))
            .is_ok()
    );
    assert!(
        app.cold_lane_verdict_gate(ApplyMode::Immediate, Some(&ticket))
            .is_ok()
    );
}

/// A READ THAT ENDED WAITING FOR AN UPDATE CHECK TO CONFIRM THE STAGE'S SIGNER
/// IS NO FAILURE OF THE COLD LANE (round eight, #503's review). The successor's
/// boot swap would wait the same way (gate 4d), so nothing is relaunched; and
/// the wait lasts until a check reads the machine roster, which no attempt does,
/// so it is a deferral (activity's error: charged nothing) and not the `Failed`
/// the synchronous arm books on the transient budget. The automatic lane holds
/// for the check with its intent kept, due when the hold next asks the stage.
///
/// RED before: the read's passing answer was the attempt's `Failed`, booked as a
/// transient physical failure — 600 s, 1800 s, then a 6 h stand-down.
#[test]
fn a_read_waiting_for_a_roster_confirmation_defers_the_cold_lane_and_holds() {
    // The new hold books its wait as the standing refusal.
    let _ledger = crate::app_update_screen::hold_update_ledger_for_test();
    let mut app = crate::App::headless_for_test();
    let target = crate::running_build_number().saturating_add(1);
    let ticket = ApplyAttemptTicket::for_test(target, COMMIT, &artifact());
    let wait = format!(
        "staged update failed pre-park verification: staged build {target} has not been \
         {} on this Mac, so a revocation of its signer could have been missed; it \
         installs once the next update check confirms it ({})",
        aterm_update::ROSTER_UNCONFIRMED_KEY,
        aterm_update::PASSING_REFUSAL_KEY
    );
    assert!(aterm_update::is_roster_confirmation_wait(&wait));
    let passing = Arc::new(std::sync::OnceLock::new());
    passing
        .set(crate::PassingPreverification {
            reason: wait.clone(),
            ended: Instant::now(),
        })
        .unwrap();
    app.handoff_preverify_in_flight = Some(crate::PreverifyInFlight {
        build: target,
        commit: COMMIT.to_string(),
        artifact: artifact(),
        deadline: Instant::now() + crate::HANDOFF_PREVERIFY_READ_CEILING,
        done: Arc::new(AtomicBool::new(true)),
        passing,
    });
    for mode in [ApplyMode::Automatic, ApplyMode::AutomaticPastGrace] {
        let error = app
            .cold_lane_verdict_gate(mode, Some(&ticket))
            .expect_err("nothing is relaunched into the same wait");
        assert!(error.is_activity_deferred(), "charged nothing: {error:?}");
        let message = error.to_string();
        assert!(
            aterm_update::is_roster_confirmation_wait(&message)
                && message.contains("not relaunched"),
            "{message}"
        );
    }
    let held = app
        .auto_apply_roster_wait
        .clone()
        .expect("the automatic lane holds for the check");
    assert_eq!(held.build, target);
    let intent = app.auto_apply_intent.expect("its intent kept");
    assert_eq!(intent.build, target);
    assert_eq!(
        intent.retry_at, held.reread_at,
        "due when the hold asks again"
    );
    assert!(app.auto_apply_manual_only.is_none(), "no latch");
    assert!(app.auto_apply_physical_retry.is_none(), "no budget spent");
    let current = app.native_updater_service.snapshot().current_build;
    assert!(
        aterm_update::is_roster_confirmation_wait(
            &aterm_update::apply_lane_report(current)
                .expect("the scratch ledger")
                .last_refusal
        ),
        "the wait is the standing refusal, not a failure"
    );
    // A PERSON'S PRESS IS NOT RELAUNCHED INTO IT EITHER (#503's second review):
    // the successor's gate 4d would wait the same way, so the press closed and
    // reopened the window and installed nothing. The wait is no verdict on the
    // bytes, so it binds a person's press too; it re-arms nothing.
    app.auto_apply_intent = None;
    let error = app
        .cold_lane_verdict_gate(ApplyMode::Immediate, Some(&ticket))
        .expect_err("a person's press is deferred, not relaunched");
    assert!(error.is_activity_deferred(), "charged nothing: {error:?}");
    assert!(
        aterm_update::is_roster_confirmation_wait(&error.to_string()),
        "and names the wait: {error}"
    );
    assert_eq!(app.auto_apply_intent, None, "a person's press arms nothing");
    // A CHECK THAT REPORTED THE STAGE SINCE ends that knowledge: it may have
    // confirmed the stage, so the read (begun before it) and the hold (answered
    // before it) answer nothing, and a person's press relaunches into the swap's
    // own gate. The automatic lane, which would read the stage again, has no
    // stage to read here and goes on; it never books the stale wait as a failure.
    app.roster_wait_check_reported(target);
    assert!(
        app.cold_lane_verdict_gate(ApplyMode::Immediate, Some(&ticket))
            .is_ok(),
        "the knowledge is stale once a check has reported the stage"
    );
    assert!(
        app.cold_lane_verdict_gate(ApplyMode::Automatic, Some(&ticket))
            .is_ok(),
        "a stale wait is never the attempt's `Failed`"
    );
}

/// A CACHED REFUSAL THAT IS ONLY THE WAIT FOR A ROSTER CONFIRMATION IS HELD,
/// NEVER BOOKED (round eight, #503's second review). A refused cold relaunch no
/// longer seeds the wait as a verdict (`ColdRelaunch::seed`), but whatever puts
/// one in the cache, the automatic cold lane's `Some(false)` arm used to answer
/// it with the `Failed` the synchronous arm books on the transient budget. It
/// holds for the check instead, its intent kept. CONTROL: any other cached
/// refusal is still the attempt's `Failed`.
#[test]
fn a_cached_roster_wait_holds_the_cold_lane_and_is_never_its_failure() {
    let _ledger = crate::app_update_screen::hold_update_ledger_for_test();
    let mut app = crate::App::headless_for_test();
    let target = crate::running_build_number().saturating_add(1);
    let ticket = ApplyAttemptTicket::for_test(target, COMMIT, &artifact());
    let cache = |app: &crate::App, reason: String| {
        *app.handoff_preverified.lock().unwrap() = Some(crate::HandoffPreverification {
            build: target,
            commit: COMMIT.to_string(),
            artifact: artifact(),
            at: Instant::now(),
            passed: false,
            reason: Some(reason),
            policy: None,
            grant_chunks: false,
        });
    };
    let wait = format!(
        "the relaunched app's install of build {target} refused: staged build {target} \
         has not been {} on this Mac, so a revocation of its signer could have been \
         missed; it installs once the next update check confirms it ({})",
        aterm_update::ROSTER_UNCONFIRMED_KEY,
        aterm_update::PASSING_REFUSAL_KEY
    );
    cache(&app, wait);
    let error = app
        .cold_lane_verdict_gate(ApplyMode::AutomaticPastGrace, Some(&ticket))
        .expect_err("nothing is relaunched into the wait");
    assert!(error.is_activity_deferred(), "charged nothing: {error:?}");
    let held = app.auto_apply_roster_wait.clone().expect("the lane holds");
    assert!(held.said && held.build == target);
    assert!(
        app.auto_apply_intent
            .is_some_and(|intent| intent.build == target && intent.retry_at == held.reread_at),
        "its intent kept, due when the hold asks again"
    );
    assert!(app.auto_apply_manual_only.is_none(), "no latch");
    // CONTROL: a verdict is still the attempt's failure.
    app.auto_apply_roster_wait = None;
    cache(&app, install_location_refusal());
    let error = app
        .cold_lane_verdict_gate(ApplyMode::AutomaticPastGrace, Some(&ticket))
        .expect_err("the verdict answers the attempt");
    assert!(
        !error.is_activity_deferred() && !error.is_refused(),
        "a `Failed`: {error:?}"
    );
}

/// Wired where it has to be: first thing in the cold arm, before the quit
/// layout is written and before the `exec`.
#[test]
fn the_verdict_is_read_before_the_layout_is_written_and_the_process_replaced() {
    let src = include_str!("app_update_handoff.rs");
    let arm = src
        .find("crate::native_update_admission::ApplyLane::Cold,\n            ) => {")
        .expect("the cold arm");
    let gate = src[arm..]
        .find("self.cold_lane_verdict_gate(mode, apply_attempt.as_ref())?;")
        .expect("the cold arm reads the verdict");
    let layout = src[arm..]
        .find(".cold_lane_layout()")
        .expect("the cold arm writes a layout");
    let exec = src[arm..]
        .find("aterm_update::exec_keeping_sigpipe(&mut command)")
        .expect("and execs");
    assert!(gate < layout && layout < exec);
    // …and hands its successor the lane's budget and the relaunch mark (#1's
    // and #7's reviews), on the command it execs.
    let budget = src[arm..]
        .find("bind_cold_lane_budget(")
        .expect("the cold arm carries the budget");
    let mark = src[arm..]
        .find(".env(aterm_update::RELAUNCH_OF_RUNNING_ENV, \"1\")")
        .expect("the cold arm marks its successor");
    assert!(budget < exec && mark < exec);
    // The seamless lane marks its successor too.
    let seamless = arm + exec;
    assert!(
        src[seamless..].contains(".env(aterm_update::RELAUNCH_OF_RUNNING_ENV, \"1\");"),
        "the seamless launch marks its successor"
    );
}

/// The successor half: a relaunch of this build whose boot swap refused hands
/// the refusal to its automatic lane as the verdict on that artifact.
#[test]
fn a_refused_cold_relaunch_answers_its_own_first_attempt() {
    use std::ffi::OsStr;
    let running = crate::build_info::build_number();
    let target = crate::running_build_number().saturating_add(1);
    let target_text = target.to_string();
    let digest = artifact();
    let relaunch = ColdRelaunch::from_values(
        Some(OsStr::new(running)),
        Some(OsStr::new(&target_text)),
        Some(OsStr::new(COMMIT)),
        Some(OsStr::new(&digest)),
        None,
        running,
    )
    .expect("the cold lane's relaunch of this build");
    // NEGATIVE CONTROLS: an update that landed (the swap re-exec names the
    // build it came FROM), a launch with no binding (a person's; the QA seam's
    // same-image relaunch binds none), a half binding.
    for (from, build) in [
        (Some("1"), Some(target_text.as_str())),
        (None, Some(target_text.as_str())),
        (Some(running), None),
    ] {
        assert_eq!(
            ColdRelaunch::from_values(
                from.map(OsStr::new),
                build.map(OsStr::new),
                Some(OsStr::new(COMMIT)),
                Some(OsStr::new(&digest)),
                None,
                running,
            ),
            None
        );
    }

    // The swap refused: the entry is a refusal of exactly that artifact.
    let refused = aterm_update::ApplyOutcome::Deferred(install_location_refusal());
    let seed = relaunch
        .seed(&refused)
        .expect("a refused swap leaves a verdict");
    assert!(!seed.verdict.passed);
    assert_eq!(
        (
            seed.verdict.build,
            seed.verdict.commit.as_str(),
            seed.verdict.artifact.as_str()
        ),
        (target, COMMIT, digest.as_str())
    );
    assert_eq!(
        seed.budget,
        super::ColdLaneBudget::default(),
        "nothing was carried, so nothing is booked before it"
    );
    // Nothing to say when nothing was refused.
    for quiet in [
        aterm_update::ApplyOutcome::NoUpdate,
        aterm_update::ApplyOutcome::NotApplicable,
    ] {
        assert!(relaunch.seed(&quiet).is_none());
    }

    // The new process's first automatic attempt is answered by it — no
    // relaunch — and the arm-time read does not replace it with a second one.
    let mut app = crate::App::headless_for_test();
    app.seed_refused_cold_relaunch(seed, Instant::now());
    assert!(
        !crate::app_native::preverify_read_needed(
            app.handoff_preverified.lock().unwrap().as_ref(),
            None,
            target,
            COMMIT,
            &digest,
            Instant::now(),
        ),
        "a fresh verdict is on record, so arming starts no read"
    );
    let ticket = ApplyAttemptTicket::for_test(target, COMMIT, &digest);
    let error = app
        .cold_lane_verdict_gate(ApplyMode::Automatic, Some(&ticket))
        .expect_err("the refusal answers the first attempt");
    assert!(
        error
            .to_string()
            .contains(aterm_update::INSTALL_LOCATION_REFUSAL_KEY),
        "{error}"
    );
    #[cfg(target_os = "macos")]
    assert!(crate::update_apply_trouble::ApplyTrouble::needs_person(
        &error.to_string()
    ));
}

/// Round eight, #1's review: the refusal answers the first automatic attempt
/// HOWEVER LATE IT COMES. The copy in the verdict cache lapses with every
/// verdict's freshness, and an attempt held back past it (a person typing in a
/// document tab keeps the automatic lane waiting up to its ladder) found no
/// verdict and relaunched into the same refusal — unbooked, so the carried
/// budget never counted it. The refusal is kept apart until an automatic read
/// takes it, and it answers exactly once. A person's apply does not take it.
#[test]
fn a_refused_cold_relaunch_answers_a_late_first_attempt_once() {
    use std::ffi::OsStr;
    let running = crate::build_info::build_number();
    let target = crate::running_build_number().saturating_add(1);
    let target_text = target.to_string();
    let digest = artifact();
    let seed = ColdRelaunch::from_values(
        Some(OsStr::new(running)),
        Some(OsStr::new(&target_text)),
        Some(OsStr::new(COMMIT)),
        Some(OsStr::new(&digest)),
        None,
        running,
    )
    .and_then(|relaunch| {
        relaunch.seed(&aterm_update::ApplyOutcome::Deferred(
            "staged candidate cannot start: dyld refused it".to_string(),
        ))
    })
    .expect("a refused cold relaunch");
    let mut app = crate::App::headless_for_test();
    app.seed_refused_cold_relaunch(seed, Instant::now());
    let lapse = |app: &crate::App| {
        let mut cached = app.handoff_preverified.lock().unwrap();
        let entry = cached.as_mut().expect("the refusal is cached");
        if let Some(then) = entry
            .at
            .checked_sub(crate::HANDOFF_PREVERIFY_FRESHNESS + std::time::Duration::from_secs(1))
        {
            entry.at = then;
        }
    };
    lapse(&app);
    let ticket = ApplyAttemptTicket::for_test(target, COMMIT, &digest);
    assert_eq!(
        app.cached_handoff_preverification(&ticket),
        None,
        "PRECONDITION: the cached copy has lapsed"
    );
    // A person's apply is not answered by it, and does not use it up.
    assert!(
        app.cold_lane_verdict_gate(ApplyMode::Immediate, Some(&ticket))
            .is_ok()
    );
    let error = app
        .cold_lane_verdict_gate(ApplyMode::AutomaticPastGrace, Some(&ticket))
        .expect_err("the refusal answers the late first automatic attempt");
    assert!(
        !error.is_refused() && !error.is_activity_deferred(),
        "a Failed the synchronous arm books: {error:?}"
    );
    assert!(error.to_string().contains("cannot start"), "{error}");
    assert!(
        app.cached_handoff_preverification_reason(&ticket)
            .is_some_and(|reason| reason.contains("cannot start")),
        "the synchronous arm that books it reads it fresh"
    );
    // Taken once: when that copy lapses too, the lane reads the bytes again.
    lapse(&app);
    let done = Arc::new(AtomicBool::new(true));
    app.handoff_preverify_in_flight = Some(crate::PreverifyInFlight {
        build: target,
        commit: COMMIT.to_string(),
        artifact: artifact(),
        deadline: Instant::now() + crate::HANDOFF_PREVERIFY_READ_CEILING,
        done,
        passing: Default::default(),
    });
    assert!(
        app.cold_lane_verdict_gate(ApplyMode::AutomaticPastGrace, Some(&ticket))
            .is_ok(),
        "answered once; a read that ended with nothing on record relaunches"
    );
}

/// Round eight, #1's review: THE BUDGET CROSSES THE `exec` INTACT, for exactly
/// the artifact it is about.
#[test]
fn the_cold_lane_budget_round_trips_for_its_own_artifact_only() {
    use super::{CarriedFailures, CarriedVerdict, ColdLaneBudget};
    let digest = artifact();
    let full = ColdLaneBudget {
        failures: Some(CarriedFailures {
            cycles: 7,
            since_secs: 21_600,
            activation: false,
        }),
        verdict: Some(CarriedVerdict {
            resample_in_secs: Some(86_000),
            activation: true,
        }),
    };
    let spent = ColdLaneBudget {
        failures: None,
        verdict: Some(CarriedVerdict {
            resample_in_secs: None,
            activation: false,
        }),
    };
    for budget in [full, spent, ColdLaneBudget::default()] {
        assert_eq!(
            ColdLaneBudget::decode(&budget.encode(11, &digest), 11, &digest),
            Some(budget)
        );
    }
    // Never another artifact's.
    let encoded = full.encode(11, &digest);
    assert_eq!(ColdLaneBudget::decode(&encoded, 12, &digest), None);
    assert_eq!(ColdLaneBudget::decode(&encoded, 11, &"cd".repeat(32)), None);
    // A half that does not parse is dropped on its own.
    assert_eq!(
        ColdLaneBudget::decode(&encoded.replace("cycles=7", "cycles=x"), 11, &digest),
        Some(ColdLaneBudget {
            failures: None,
            ..full
        })
    );
    // The relaunch reads it with the binding it belongs to.
    let running = crate::build_info::build_number();
    let relaunch = ColdRelaunch::from_values(
        Some(std::ffi::OsStr::new(running)),
        Some(std::ffi::OsStr::new("11")),
        Some(std::ffi::OsStr::new(COMMIT)),
        Some(std::ffi::OsStr::new(&digest)),
        Some(std::ffi::OsStr::new(&encoded)),
        running,
    )
    .expect("the cold lane's relaunch");
    let seed = relaunch
        .seed(&aterm_update::ApplyOutcome::Deferred("refused".to_string()))
        .expect("a refusal");
    assert_eq!(seed.budget, full);
    assert_eq!(seed.dmg_sha256, Some([0xab; 32]));
}

/// Round eight, #1's review (P4): THE WIRING, pinned where it lives. The launch
/// reads whether it is the cold lane's relaunch BEFORE the boot apply consumes
/// the binding, turns the apply's outcome into the seed after it, and hands the
/// seed to the `App` before the startup reconcile can arm the automatic lane. A
/// test that builds the seeded `App` by hand stays green without any of it.
#[test]
fn main_entry_seeds_the_refusal_before_the_lane_can_arm() {
    let src = include_str!("lib.rs");
    let entry = src.find("pub fn main_entry(").expect("the window's entry");
    let body = &src[entry..];
    let at = |needle: &str| {
        body.find(needle)
            .unwrap_or_else(|| panic!("main_entry has `{needle}`"))
    };
    let read = at("app_update_handoff::ColdRelaunch::of_this_launch(");
    let apply = at("aterm_update::apply_staged_if_ready_preserving_fds_exact(");
    let seed = at("cold_relaunch.and_then(|relaunch| relaunch.seed(&boot_apply))");
    let app = at("let mut app = App {");
    let handed = at("app.seed_refused_cold_relaunch(seed, Instant::now());");
    let reconcile = at(
        "app.request_native_update_reconcile(app_native::NativeUpdateReconcilePurpose::Startup)",
    );
    assert!(
        read < apply && apply < seed && seed < app && app < handed && handed < reconcile,
        "read {read} < apply {apply} < seed {seed} < App {app} < handed {handed} < \
         startup reconcile {reconcile}"
    );
    assert!(
        body[app..handed].contains("handoff_preverified: std::sync::Arc::default(),"),
        "the App starts with no verdict of its own; the seed is the one"
    );
}
