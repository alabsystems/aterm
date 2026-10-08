// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use super::*;
use aterm_update_core::pkg_check::{
    FULL_PASS_INTERVAL_SECS, PASS_SPACING_SECS, PassOutcome, Stamps,
};

const NOW: i64 = 1_790_000_000;
const HOUR: i64 = 3600;
const WALK: i64 = FULL_PASS_INTERVAL_SECS as i64;

/// A record whose last full pass ended at `attempt`: the success itself (it was `ok`),
/// or a pass that recorded a failure.
fn record(success: Option<i64>, attempt: Option<i64>, build: u64) -> Stamps {
    Stamps {
        last_success: success,
        last_attempt: attempt,
        last_outcome: attempt.map(|attempt| {
            if success == Some(attempt) {
                PassOutcome::Ok
            } else {
                PassOutcome::Failed
            }
        }),
        last_index_build: build,
        last_pass_target: None,
        in_flight: false,
    }
}

/// A record checked `ago` seconds before now, index `build` verified.
fn checked(ago: i64, build: u64) -> Stamps {
    record(Some(NOW - ago), None, build)
}

fn gate(
    why: PassWhy,
    stamps: &Stamps,
    own: OwnPasses,
    ran_at: Option<i64>,
) -> Option<(Duration, PassWhy)> {
    scheduled_pass_gate(why, stamps, own, ran_at, NOW)
}

/// An owed launch and a stub's wish always run, even over a sibling's installing pass
/// (they queue at the store lock).
#[test]
fn a_forced_launch_and_a_stubs_wish_always_run() {
    let installing = Stamps {
        in_flight: true,
        ..checked(10, 42)
    };
    for why in [PassWhy::Launch, PassWhy::Bumped] {
        assert_eq!(
            gate(why, &installing, OwnPasses::default(), None),
            None,
            "{why:?}"
        );
    }
}

/// A sibling's installing or recently attempted pass holds every other scheduled pass
/// until the next look (a published build rechecks a live holder in 5 s). An attempt
/// no later than this lane's own last child is this lane's (negative control).
#[test]
fn a_siblings_pass_in_flight_or_a_moment_old_holds_every_scheduled_pass() {
    let installing = Stamps {
        in_flight: true,
        ..checked(3 * HOUR, 42)
    };
    let sibling_attempt = record(Some(NOW - 7 * HOUR), Some(NOW - 60), 42);
    for why in [
        PassWhy::Published(43),
        PassWhy::Retry,
        PassWhy::Walk,
        PassWhy::LaunchDue,
    ] {
        let in_flight_park = if matches!(why, PassWhy::Published(_)) {
            PUBLISHED_HOLDER_RECHECK
        } else {
            Duration::from_secs(PASS_SPACING_SECS)
        };
        assert_eq!(
            gate(why, &installing, OwnPasses::default(), None),
            Some((in_flight_park, why)),
            "{why:?} behind a pass installing"
        );
        assert_eq!(
            gate(why, &sibling_attempt, OwnPasses::default(), None),
            Some((Duration::from_secs(PASS_SPACING_SECS - 60), why)),
            "{why:?} a minute after a sibling's attempt"
        );
    }
    // Our own attempt a minute ago is no sibling's: the walk is owed on the record.
    assert_eq!(
        gate(
            PassWhy::LaunchDue,
            &sibling_attempt,
            OwnPasses::default(),
            Some(NOW - 30)
        ),
        None
    );
}

#[test]
fn published_45_runs_after_sibling_checked_44_but_same_or_failed_attempts_still_space() {
    let model = aterm_spec::derive::atpkg_published_spacing_model();
    let mut modeled = model.init_state();
    for action in ["EndOldOk", "Publish45", "DecideGui"] {
        assert!(model.fire(action, &mut modeled), "{action}: {modeled:?}");
    }
    let older_success = Stamps {
        last_pass_target: Some((NOW - 60, 44)),
        ..record(Some(NOW - 60), Some(NOW - 60), 44)
    };
    let actual = gate(
        PassWhy::Published(45),
        &older_success,
        OwnPasses::default(),
        None,
    );
    assert_eq!(actual.is_none(), modeled["gui"] == 1);
    assert_eq!(actual, None, "a newly published build runs now");
    assert_eq!(
        gate(
            PassWhy::Published(44),
            &older_success,
            OwnPasses::default(),
            None
        ),
        Some((
            Duration::from_secs(PASS_SPACING_SECS - 60),
            PassWhy::Published(44)
        )),
        "the same build remains deduplicated"
    );
    let failed = Stamps {
        last_pass_target: Some((NOW - 60, 44)),
        ..record(Some(NOW - 7 * HOUR), Some(NOW - 60), 44)
    };
    assert_eq!(
        gate(PassWhy::Published(45), &failed, OwnPasses::default(), None),
        None,
        "a newer 45 can run after the holder failed on 44"
    );
    let failed_same_target = Stamps {
        last_pass_target: Some((NOW - 60, 45)),
        ..failed
    };
    assert_eq!(
        gate(
            PassWhy::Published(45),
            &failed_same_target,
            OwnPasses::default(),
            None
        ),
        Some((
            Duration::from_secs(PASS_SPACING_SECS - 60),
            PassWhy::Published(45)
        )),
        "the holder already failed on 45, so the sibling must wait"
    );
    let unknown = record(Some(NOW - 60), Some(NOW - 60), 44);
    assert_eq!(
        gate(PassWhy::Published(45), &unknown, OwnPasses::default(), None),
        Some((
            Duration::from_secs(PASS_SPACING_SECS - 60),
            PassWhy::Published(45)
        )),
        "older records fail closed"
    );
    let installing = Stamps {
        in_flight: true,
        ..older_success
    };
    assert_eq!(
        gate(
            PassWhy::Published(45),
            &installing,
            OwnPasses::default(),
            None
        ),
        Some((PUBLISHED_HOLDER_RECHECK, PassWhy::Published(45))),
        "a live holder owns the store, but its completion is rechecked promptly"
    );
    assert_eq!(
        gate(
            PassWhy::Published(45),
            &older_success,
            OwnPasses::default(),
            None
        ),
        None,
        "after the older holder ends, the same published wake runs"
    );
}

/// A published index a sibling already landed is not run again; the walk resumes.
/// Negative control: an index above the floor runs.
#[test]
fn a_published_index_a_sibling_landed_is_the_walk_not_a_pass() {
    let landed = checked(20 * 60, 43);
    assert_eq!(
        gate(PassWhy::Published(43), &landed, OwnPasses::default(), None),
        Some((Duration::from_secs((WALK - 20 * 60) as u64), PassWhy::Walk))
    );
    assert_eq!(
        gate(PassWhy::Published(44), &landed, OwnPasses::default(), None),
        None
    );
}

/// A launch parked behind a sibling whose pass succeeded owes nothing and returns to
/// the walk. Negative controls: the sibling failed, the record is stale, or never
/// checked: the launch runs.
#[test]
fn a_launch_pass_a_sibling_covered_while_it_parked_is_the_walk() {
    let none = OwnPasses::default();
    let stale = checked(10 * HOUR, 42);
    let installing = Stamps {
        in_flight: true,
        ..stale
    };
    assert_eq!(
        gate(PassWhy::LaunchDue, &installing, none, None),
        Some((Duration::from_secs(PASS_SPACING_SECS), PassWhy::LaunchDue))
    );
    let spacing = PASS_SPACING_SECS as i64;
    let covered = checked(spacing + 30, 42);
    assert_eq!(
        gate(PassWhy::LaunchDue, &covered, none, None),
        Some((
            Duration::from_secs((WALK - spacing - 30) as u64),
            PassWhy::Walk
        ))
    );
    let failed = record(Some(NOW - 10 * HOUR), Some(NOW - spacing - 30), 42);
    assert_eq!(gate(PassWhy::LaunchDue, &failed, none, None), None);
    assert_eq!(gate(PassWhy::LaunchDue, &stale, none, None), None);
    assert_eq!(
        gate(PassWhy::LaunchDue, &Stamps::default(), none, None),
        None,
        "never checked"
    );
}

/// A retry healed by a sibling's later success is done; the walk takes over.
/// Negative control: no success since the failure, the retry runs.
#[test]
fn a_retry_a_siblings_success_healed_is_done() {
    let own = OwnPasses {
        ok_at: None,
        failed_at: Some(NOW - HOUR),
    };
    let healed = checked(20 * 60, 42);
    assert_eq!(
        gate(PassWhy::Retry, &healed, own, Some(NOW - HOUR)),
        Some((Duration::from_secs((WALK - 20 * 60) as u64), PassWhy::Walk))
    );
    let still_failing = record(Some(NOW - 9 * HOUR), Some(NOW - HOUR), 42);
    assert_eq!(
        gate(PassWhy::Retry, &still_failing, own, Some(NOW - HOUR)),
        None
    );
}

/// The walk is machine-wide: a sibling's recent success defers it, this lane's own
/// success counts when the record is unreadable, a six-hour-old success runs it. A
/// sibling's failure never pushes the walk back.
#[test]
fn the_walk_is_owed_on_the_machine_wide_rule() {
    let none = OwnPasses::default();
    assert_eq!(
        gate(PassWhy::Walk, &checked(3 * HOUR, 42), none, None),
        Some((Duration::from_secs((3 * HOUR) as u64), PassWhy::Walk))
    );
    assert_eq!(gate(PassWhy::Walk, &checked(WALK, 42), none, None), None);
    let unreadable = Stamps::default();
    assert_eq!(
        gate(PassWhy::Walk, &unreadable, none, None),
        None,
        "never checked"
    );
    let ours = OwnPasses {
        ok_at: Some(NOW - HOUR),
        failed_at: None,
    };
    assert_eq!(
        gate(PassWhy::Walk, &unreadable, ours, Some(NOW - HOUR)),
        Some((Duration::from_secs((WALK - HOUR) as u64), PassWhy::Walk)),
        "this lane's own success, where the record says nothing"
    );
    let sibling_failed = record(Some(NOW - 9 * HOUR), Some(NOW - HOUR), 42);
    assert_eq!(gate(PassWhy::Walk, &sibling_failed, none, None), None);
    let ours_then_a_sibling_failure = record(Some(NOW - 4 * HOUR), Some(NOW - HOUR), 42);
    assert_eq!(
        gate(
            PassWhy::Walk,
            &ours_then_a_sibling_failure,
            OwnPasses {
                ok_at: Some(NOW - 4 * HOUR),
                failed_at: None
            },
            None
        ),
        Some((Duration::from_secs((WALK - 4 * HOUR) as u64), PassWhy::Walk)),
        "the walk counts from the success, not from the sibling's failure after it"
    );
}

/// What a park that ended asks for next: the same pass after contention, the retry
/// after a failure park, the walk after a healthy one — and a wish or a published
/// index whatever the park was.
#[test]
fn a_park_ending_names_the_next_pass() {
    let published = PassWhy::Published(43);
    assert_eq!(published.after_park(true, false), published);
    assert_eq!(published.after_park(false, true), PassWhy::Retry);
    assert_eq!(published.after_park(false, false), PassWhy::Walk);
    assert_eq!(ParkEnd::Elapsed.pass_why(PassWhy::Retry), PassWhy::Retry);
    assert_eq!(
        ParkEnd::StoreUnlocked.pass_why(PassWhy::Published(44)),
        PassWhy::Published(44),
        "release keeps the same requested build for the durable dedup gate"
    );
    assert_eq!(ParkEnd::Bumped.pass_why(PassWhy::Retry), PassWhy::Bumped);
    assert_eq!(
        ParkEnd::IndexPublished(44).pass_why(PassWhy::Walk),
        PassWhy::Published(44)
    );
    assert_eq!(
        ParkEnd::IndexPublished(44).pass_why(PassWhy::Published(45)),
        PassWhy::Published(45),
        "a stale probe must not downgrade the parked new build"
    );
    assert_eq!(
        ParkEnd::IndexPublished(46).pass_why(PassWhy::Published(45)),
        PassWhy::Published(46),
        "a newer probe raises the parked build"
    );
}
