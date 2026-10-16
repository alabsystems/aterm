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

/// A read of `(build, COMMIT, artifact)` started now and still running —
/// the arm-time read the spacing outran — and the flag that ends it.
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

    // Every attempt while the read runs defers — not only the first.
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
    // A person's apply waits too, so it never runs the Full capture under an unread policy. It
    // is held, not deferred (a person's deferral never comes back under
    // `[update] auto_apply = false`), naming this target and ending within the read's ceiling.
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

    // The read ends: its verdict is published, then its flag raised.
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

    // The next attempt parks — under the policy the read answered.
    let policy = app.park_policy(Some(&ticket));
    assert!(policy.known, "the policy is known by the next park");
    assert_eq!(policy.carry_ceiling(), CarryCeiling::Repaint);
    assert!(
        app.fork_lane_policy_wait(ApplyMode::Automatic, target, !policy.known)
            .is_none(),
        "a known policy needs no wait"
    );

    // Wired where the fork lane parks: before its readers stop.
    let src = include_str!("app_update_handoff.rs");
    let wait_at = src
        .find("if let Some(wait) = self.fork_lane_policy_wait(mode, target_build, policy_unread)")
        .expect("the fork lane asks");
    let park_at = src[wait_at..]
        .find("if !self.park_all_readers(deadline) {")
        .expect("and parks after");
    assert!(park_at > 0);
}

#[test]
fn a_read_that_ended_is_absent_or_outran_its_ceiling_is_not_waited_for() {
    let mut app = crate::App::headless_for_test();
    let running = crate::running_build_number();

    // Ended with nothing on record (a check that did not finish).
    let target = running.saturating_add(1);
    read_in_flight(&mut app, target).store(true, Ordering::Release);
    assert!(
        app.fork_lane_policy_wait(ApplyMode::Automatic, target, true)
            .is_none(),
        "a read that ended with nothing: the fork lane parks as it always did"
    );

    // No read at all: none could start (a headless app stages nothing).
    let target = running.saturating_add(2);
    app.handoff_preverify_in_flight = None;
    assert!(
        app.fork_lane_policy_wait(ApplyMode::Automatic, target, true)
            .is_none(),
        "nothing is being read, so nothing is waited for"
    );
    assert!(app.handoff_preverify_in_flight.is_none());

    // Still running, but this target's ceiling is spent, which the verifier's
    // bound says only an unscheduled thread can reach
    // (`the_ceiling_outlasts_every_instant_a_read_can_hold_the_lock`): the wait
    // ends and the park proceeds.
    let target = running.saturating_add(3);
    read_in_flight(&mut app, target);
    let Some(spent) = Instant::now().checked_sub(Duration::from_millis(1)) else {
        return;
    };
    app.preverify_read_waited = Some((target, spent));
    assert!(
        app.fork_lane_policy_wait(ApplyMode::Automatic, target, true)
            .is_none(),
        "the wait is bounded: past its ceiling the fork lane parks"
    );
    // …and a read for another target is not this target's.
    let other = running.saturating_add(4);
    assert!(
        app.fork_lane_policy_wait(ApplyMode::Automatic, other, true)
            .is_none(),
        "only a read of this target is waited for"
    );
}

/// A pre-verification holds or queues for the staged apply lock, so the fork lane must not park
/// while a same-key read can still hold it: the ceiling exceeds aterm-update's
/// `HANDOFF_PREVERIFY_BOUND` (the lock wait plus the verification budget) and is derived from it.
#[test]
fn the_ceiling_outlasts_every_instant_a_read_can_hold_the_lock() {
    assert_eq!(
        crate::HANDOFF_PREVERIFY_READ_CEILING,
        aterm_update::HANDOFF_PREVERIFY_BOUND + crate::HANDOFF_PREVERIFY_READ_MARGIN,
        "the ceiling is derived from the verifier's bound, not restated"
    );
    assert!(crate::HANDOFF_PREVERIFY_READ_MARGIN > Duration::ZERO);

    let mut app = crate::App::headless_for_test();
    let target = crate::running_build_number().saturating_add(1);
    // The wait and the read began one whole bound ago: this is the last
    // instant the read can still be inside its lock wait and its budget.
    let now = Instant::now();
    if now
        .checked_sub(aterm_update::HANDOFF_PREVERIFY_BOUND)
        .is_none()
    {
        return;
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
    // Once the read ends, the lane parks under whatever it published.
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

    // Wired: the spawn asks, records what it started, and its thread
    // raises the flag on every path out.
    let src = include_str!("app_native.rs");
    let body = &src[src
        .find("pub(crate) fn spawn_staged_handoff_preverification(")
        .expect("the spawn")..];
    let body = &body[..body.find("\n    }\n").expect("its end")];
    assert!(body.contains("if !preverify_read_needed("));
    assert!(body.contains("let _ends = RaiseOnDrop(done);"));
    assert!(body.contains("Ok(_) => self.handoff_preverify_in_flight = Some(flight),"));
    // A read that ends without a verdict keeps its moment on the flight, never in the verdict
    // cache, and before it raises the flag.
    let passing = body
        .find("if aterm_update::is_passing_refusal(error) {")
        .expect("the passing branch");
    let branch = &body[passing..];
    let branch = &branch[..branch.find("return;").expect("it returns")];
    assert!(branch.contains("passing.set(crate::PassingPreverification {"));
    assert!(!branch.contains("slot"), "never the verdict cache");
}
