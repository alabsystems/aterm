// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use crate::native_updater_service::{ApplyAttemptTicket, ApplyMode};
use aterm_update_core::handoff_policy::{CarryCeiling, HandoffPolicy};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::{Duration, Instant};

const COMMIT: &str = "0123456789abcdef0123456789abcdef01234567";

fn artifact() -> String {
    "ab".repeat(32)
}

fn read_in_flight(app: &mut crate::App, build: u64) -> Arc<AtomicBool> {
    let done = Arc::new(AtomicBool::new(false));
    app.handoff_preverify_in_flight = Some(crate::PreverifyInFlight {
        build,
        commit: COMMIT.to_string(),
        artifact: artifact(),
        deadline: Instant::now() + crate::HANDOFF_PREVERIFY_READ_CEILING,
        done: Arc::clone(&done),
        passing: Default::default(),
    });
    done
}

fn unread(app: &crate::App, ticket: &ApplyAttemptTicket) -> bool {
    !app.park_policy(Some(ticket)).known
}

#[test]
fn the_fork_lane_waits_while_the_policy_read_runs_and_parks_under_its_answer() {
    let mut app = crate::App::headless_for_test();
    let target = crate::running_build_number().saturating_add(1);
    let ticket = ApplyAttemptTicket::for_test(target, COMMIT, &artifact());
    let done = read_in_flight(&mut app, target);
    assert!(unread(&app, &ticket));

    for mode in [
        ApplyMode::Automatic,
        ApplyMode::Automatic,
        ApplyMode::AutomaticPastGrace,
    ] {
        let wait = app
            .fork_lane_policy_wait(mode, target, unread(&app, &ticket))
            .expect("an automatic attempt waits while the policy is being read");
        assert!(
            wait.stops_routinely(mode),
            "as routine activity, never a failure: {wait}"
        );
    }
    let before = Instant::now();
    let wait = app
        .fork_lane_policy_wait(ApplyMode::Immediate, target, unread(&app, &ticket))
        .expect("a person's apply waits while the policy is being read");
    assert!(!wait.is_activity_deferred(), "held, not deferred: {wait:?}");
    assert!(
        !wait.stops_routinely(ApplyMode::Immediate),
        "never a routine stop: {wait}"
    );
    let held = wait.held_for_read_of().expect("a person's press is held");
    assert_eq!(held.target_build, target);
    assert!(
        held.until > before && held.until <= Instant::now() + crate::HANDOFF_PREVERIFY_READ_CEILING,
        "bounded by the read's ceiling"
    );

    *app.handoff_preverified
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner) = Some(crate::HandoffPreverification {
        build: target,
        commit: COMMIT.to_string(),
        artifact: artifact(),
        at: Instant::now(),
        passed: true,
        reason: None,
        policy: Some(HandoffPolicy::parse("schema = 1\ncarry = \"repaint\"\n").expect("schema 1")),
        grant_chunks: false,
    });
    done.store(true, Ordering::Release);

    let policy = app.park_policy(Some(&ticket));
    assert!(policy.known, "the policy is known by the next park");
    assert_eq!(policy.carry_ceiling(), CarryCeiling::Repaint);
    assert!(
        app.fork_lane_policy_wait(ApplyMode::Automatic, target, !policy.known)
            .is_none(),
        "a known policy needs no wait"
    );
}

#[test]
fn a_read_that_ended_is_absent_or_outran_its_ceiling_is_not_waited_for() {
    let mut app = crate::App::headless_for_test();
    let running = crate::running_build_number();

    let target = running.saturating_add(1);
    read_in_flight(&mut app, target).store(true, Ordering::Release);
    assert!(
        app.fork_lane_policy_wait(ApplyMode::Automatic, target, true)
            .is_none(),
        "a read that ended with nothing: the fork lane parks as it always did"
    );

    let target = running.saturating_add(2);
    app.handoff_preverify_in_flight = None;
    assert!(
        app.fork_lane_policy_wait(ApplyMode::Automatic, target, true)
            .is_none(),
        "nothing is being read, so nothing is waited for"
    );
    assert!(app.handoff_preverify_in_flight.is_none());

    let target = running.saturating_add(3);
    read_in_flight(&mut app, target);
    let Some(spent) = Instant::now().checked_sub(Duration::from_millis(1)) else {
        aterm_spec::host::could_not_run("the monotonic clock here cannot reach back 1 ms");
    };
    app.preverify_read_waited = Some((target, spent));
    assert!(
        app.fork_lane_policy_wait(ApplyMode::Automatic, target, true)
            .is_none(),
        "the wait is bounded: past its ceiling the fork lane parks"
    );
    let other = running.saturating_add(4);
    assert!(
        app.fork_lane_policy_wait(ApplyMode::Automatic, other, true)
            .is_none(),
        "only a read of this target is waited for"
    );
}

#[test]
fn the_ceiling_outlasts_every_instant_a_read_can_hold_the_lock() {
    let mut app = crate::App::headless_for_test();
    let target = crate::running_build_number().saturating_add(1);
    let now = Instant::now();
    if now
        .checked_sub(aterm_update::HANDOFF_PREVERIFY_BOUND)
        .is_none()
    {
        aterm_spec::host::could_not_run("the monotonic clock here cannot reach back one bound");
    }
    let until = now + crate::HANDOFF_PREVERIFY_READ_MARGIN;
    let done = Arc::new(AtomicBool::new(false));
    app.handoff_preverify_in_flight = Some(crate::PreverifyInFlight {
        build: target,
        commit: COMMIT.to_string(),
        artifact: artifact(),
        deadline: until,
        done: Arc::clone(&done),
        passing: Default::default(),
    });
    app.preverify_read_waited = Some((target, until));
    let deferred = app.fork_lane_policy_wait(ApplyMode::Automatic, target, true);
    assert!(
        deferred.is_some(),
        "a same-key read that may still hold the apply lock: the fork lane does not park"
    );
    assert!(
        app.handoff_preverify_in_flight
            .as_ref()
            .is_some_and(|read| Arc::ptr_eq(&read.done, &done)),
        "and no second read is started beside it"
    );
    done.store(true, Ordering::Release);
    assert!(
        app.fork_lane_policy_wait(ApplyMode::Automatic, target, true)
            .is_none()
    );
}

#[test]
fn a_read_in_flight_is_not_started_twice() {
    use crate::app_native::preverify_read_needed;
    let target = crate::running_build_number().saturating_add(1);
    let now = Instant::now();
    let artifact = artifact();
    let flight = |build: u64, commit: &str, artifact: &str, deadline: Instant, ended: bool| {
        crate::PreverifyInFlight {
            build,
            commit: commit.to_string(),
            artifact: artifact.to_string(),
            deadline,
            done: Arc::new(AtomicBool::new(ended)),
            passing: Default::default(),
        }
    };
    let later = now + crate::HANDOFF_PREVERIFY_READ_CEILING;
    assert!(preverify_read_needed(
        None, None, target, COMMIT, &artifact, now
    ));
    assert!(
        !preverify_read_needed(
            None,
            Some(&flight(target, COMMIT, &artifact, later, false)),
            target,
            COMMIT,
            &artifact,
            now,
        ),
        "the same key still in flight: no second codesign beside it"
    );
    for (read, why) in [
        (
            flight(target, COMMIT, &artifact, later, true),
            "a read that ended",
        ),
        (
            flight(target, COMMIT, &artifact, now, false),
            "one past its ceiling",
        ),
        (
            flight(target + 1, COMMIT, &artifact, later, false),
            "another build",
        ),
        (
            flight(target, &"f".repeat(40), &artifact, later, false),
            "another commit",
        ),
        (
            flight(target, COMMIT, &"cd".repeat(32), later, false),
            "another artifact",
        ),
    ] {
        assert!(
            preverify_read_needed(None, Some(&read), target, COMMIT, &artifact, now),
            "{why} does not stand in for a read of this key"
        );
    }
    let fresh = crate::HandoffPreverification {
        build: target,
        commit: COMMIT.to_string(),
        artifact: artifact.clone(),
        at: now,
        passed: true,
        reason: None,
        policy: None,
        grant_chunks: false,
    };
    assert!(!preverify_read_needed(
        Some(&fresh),
        None,
        target,
        COMMIT,
        &artifact,
        now
    ));
}
