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
    verdict(&app, target, false, Some(install_location_refusal()));
    done.store(true, Ordering::Release);
    assert!(
        app.cold_lane_verdict_gate(ApplyMode::Automatic, Some(&ticket))
            .is_err_and(|error| !error.is_activity_deferred())
    );
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
            aterm_update::is_passing_refusal(&message),
            "the read's own words, a passing moment: {message}"
        );
        assert!(
            !crate::update_apply_trouble::ApplyTrouble::needs_person(&message),
            "no person's to clear: {message}"
        );
    }
    assert_eq!(app.cached_handoff_preverification(&ticket), None);
    assert_eq!(app.cached_handoff_refusal_reason(target), None);
    assert_eq!(
        crate::app_native::PhysicalFailureShape::cached_refusal_shape(None),
        crate::app_native::PhysicalFailureShape::Transient
    );

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
    verdict(&app, target, true, None);
    done.store(true, Ordering::Release);
    assert!(
        app.cold_lane_verdict_gate(ApplyMode::Automatic, Some(&ticket))
            .is_ok()
    );

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

#[cfg(target_os = "macos")]
#[test]
fn a_read_waiting_for_a_roster_confirmation_defers_the_cold_lane_and_holds() {
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
            aterm_update::is_roster_confirmation_wait(&message),
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
    for quiet in [
        aterm_update::ApplyOutcome::NoUpdate,
        aterm_update::ApplyOutcome::NotApplicable,
    ] {
        assert!(relaunch.seed(&quiet).is_none());
    }

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
    let encoded = full.encode(11, &digest);
    assert_eq!(ColdLaneBudget::decode(&encoded, 12, &digest), None);
    assert_eq!(ColdLaneBudget::decode(&encoded, 11, &"cd".repeat(32)), None);
    assert_eq!(
        ColdLaneBudget::decode(&encoded.replace("cycles=7", "cycles=x"), 11, &digest),
        Some(ColdLaneBudget {
            failures: None,
            ..full
        })
    );
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
