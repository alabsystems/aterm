// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use super::*;
use codex_rotation::NoHandoff;

fn intent() -> codex_renewal::Intent {
    codex_renewal::Intent {
        purpose: codex_renewal::Purpose::WeeklyQuota,
        thread: "00000000-0000-0000-0000-000000000001".into(),
        wall: "00000000-0000-0000-0000-000000000002".into(),
        identity: codex_renewal::Identity::from_native("source", "user"),
    }
}

type Read = Result<Option<codex_rotation::Proposal<ExecCommand>>, &'static str>;

/// The progress once a job's first read, come back at `at`, refused the session for `why`.
fn refused(why: &'static str, at: Instant) -> Progress {
    progress_after_observation(&Progress::Reading(0), Err(why), true, at)
}

/// A READ'S REFUSAL THAT STANDS ENDS THE ASKING, UNDER ITS OWN KIND AND TIME: each refusal the
/// pool says stands becomes the answer that no handoff is coming, of the kind the pool names,
/// decided when the read came back and the first such read of its wait. A read the session
/// moved on from, a refusal that does not stand and a read that selected nothing are asked again
/// shortly.
#[test]
fn a_standing_refusal_is_kept_with_its_kind_and_the_time_it_was_read() {
    let now = Instant::now();
    for (why, end) in [
        ("not-walled", NoHandoff::NoWallToLeave),
        ("source-unbound", NoHandoff::NoRowOfThePool),
        ("source-missing", NoHandoff::NoRowOfThePool),
        ("source-unmanaged", NoHandoff::NoRowOfThePool),
        ("source-names-another", NoHandoff::LoginNamesAnother),
    ] {
        let first = NoneComing {
            end,
            decided: now,
            reads: 1,
            asked: now,
        };
        assert!(
            matches!(refused(why, now), Progress::NotNeeded(none) if none == first),
            "{why}"
        );
        assert!(
            matches!(
                progress_after_observation(&Progress::Reading(0), Err(why), false, now),
                Progress::Retry(at) if at > now
            ),
            "stale {why}"
        );
    }
    let again: [Read; 3] = [Err("roster-busy"), Err("probe-panicked"), Ok(None)];
    for result in again {
        assert!(matches!(
            progress_after_observation(&Progress::Reading(0), result, true, now),
            Progress::Retry(at) if at > now
        ));
    }
}

/// NO WALL TO LEAVE IS READ AGAIN ONCE THE WALL IT JUDGED HAS RESET: an exec run is one job at
/// every wall it meets, so the answer that its login stood at no wall a handoff leaves answers
/// its asks only until the nearest reset that answer waited out, and its grace, have passed
/// (`RESET_SOON_S + RESET_GRACE_S`). A moment inside that time the ask is answered `not-needed`
/// and nothing is read; at it, the ask starts a read and is answered `waiting`, as is each ask
/// while that read is out, which starts no second one.
#[test]
fn no_wall_to_leave_stands_until_its_wall_has_reset_and_is_then_read_again() {
    assert_eq!(
        NO_WALL_STANDS,
        Duration::from_secs((accounts::RESET_SOON_S + accounts::RESET_GRACE_S).unsigned_abs())
    );
    let decided = Instant::now();
    let intent = intent();
    let mut progress = refused("not-walled", decided);
    for inside in [Duration::ZERO, NO_WALL_STANDS - Duration::from_millis(1)] {
        assert_eq!(
            answer(&mut progress, &intent, decided + inside),
            (NOT_NEEDED.to_owned(), false),
            "{inside:?}"
        );
        assert!(
            matches!(
                progress,
                Progress::NotNeeded(NoneComing { end: NoHandoff::NoWallToLeave, decided: at, .. })
                    if at == decided
            ),
            "{inside:?}: the answer stands as it was decided"
        );
    }
    let past = decided + NO_WALL_STANDS;
    assert_eq!(
        answer(&mut progress, &intent, past),
        (WAITING.to_owned(), true),
        "the first ask past it reads the login again"
    );
    assert!(matches!(progress, Progress::Reading(_)));
    assert_eq!(
        answer(&mut progress, &intent, past + Duration::from_secs(5)),
        (WAITING.to_owned(), false),
        "one read at a time"
    );
}

/// The seconds from each read's answer to the next read's start, for `reads` reads, of a session
/// that asks every five seconds from `from` on and whose login reads as no wall to leave each
/// time; `progress` is what it asks of.
fn seconds_between_reads(progress: &mut Progress, from: Instant, reads: usize) -> Vec<u64> {
    let intent = intent();
    let mut gaps = Vec::new();
    let (mut now, mut decided) = (from, from);
    while gaps.len() < reads {
        now += Duration::from_secs(5);
        let (reply, starts) = answer(progress, &intent, now);
        if starts {
            assert_eq!(reply, WAITING);
            gaps.push((now - decided).as_secs());
            *progress = progress_after_observation(progress, Err("not-walled"), true, now);
            decided = now;
        } else {
            assert_eq!(reply, NOT_NEEDED, "{:?} in", now - decided);
        }
    }
    gaps
}

/// A WAIT THAT KEEPS READING AS NO WALL IS READ BY RUNGS, NOT AT A FLAT PACE: a session that
/// asks every five seconds, and whose login reads as no wall to leave each time, is read again
/// `RESET_SOON_S + RESET_GRACE_S` after the first read, then by the rungs a wall with no reset
/// stands by (30 minutes, 2 hours, 6 hours, 6 hours), and is answered `not-needed` at every ask
/// between. A flat stand would read its provider about 82 times a day for as long as it asks.
#[test]
fn a_wait_that_keeps_reading_as_no_wall_is_read_by_rungs() {
    let decided = Instant::now();
    let mut progress = refused("not-walled", decided);
    let [first, second, last] = accounts::NO_RESET_WALL_S.map(i64::unsigned_abs);
    assert_eq!(
        seconds_between_reads(&mut progress, decided, 5),
        [NO_WALL_STANDS.as_secs(), first, second, last, last]
    );
}

/// A SESSION THAT STOPPED ASKING WAITS AT ANOTHER WALL, FROM THE FIRST STAND: the stands one
/// wait climbed to are that wait's. A session whose asks were all within `STILL_ASKING` of the
/// one before is still inside its stand; one that then stops asking for longer than that, and
/// asks again, ran in between: its ask is judged by the first stand
/// (`RESET_SOON_S + RESET_GRACE_S` from the answer), so it is read at once here, and its new
/// wait climbs from the first rung again.
#[test]
fn a_session_that_stopped_asking_is_read_from_the_first_stand_again() {
    let decided = Instant::now();
    let intent = intent();
    let mut progress = refused("not-walled", decided);
    // Three reads in a row said no wall: the answer in hand stands two hours.
    seconds_between_reads(&mut progress, decided, 2);
    let Progress::NotNeeded(NoneComing { decided, asked, .. }) = progress else {
        panic!("{progress:?}");
    };
    assert_eq!(asked, decided);
    let step = STILL_ASKING;
    let mut now = decided;
    while now - decided < NO_WALL_STANDS + step {
        now += step;
        assert_eq!(
            answer(&mut progress, &intent, now),
            (NOT_NEEDED.to_owned(), false),
            "still asking, {:?} into a two-hour stand",
            now - decided
        );
    }
    now += step + Duration::from_millis(1);
    assert_eq!(
        answer(&mut progress, &intent, now),
        (WAITING.to_owned(), true),
        "asked again after stopping, past the first stand"
    );
    progress = progress_after_observation(&progress, Err("not-walled"), true, now);
    assert_eq!(
        seconds_between_reads(&mut progress, now, 2),
        [
            NO_WALL_STANDS.as_secs(),
            accounts::NO_RESET_WALL_S[0].unsigned_abs()
        ],
        "the new wait climbs from the first rung"
    );
}

/// WHAT NO WAIT CHANGES STANDS AT ANY AGE: a login that is no row of the pool, or that names
/// another account than its row's, is answered `not-needed` however long ago it was read, and
/// starts no read.
#[test]
fn a_login_no_wait_changes_is_answered_not_needed_at_any_age() {
    let decided = Instant::now();
    let intent = intent();
    for (why, end) in [
        ("source-unbound", NoHandoff::NoRowOfThePool),
        ("source-names-another", NoHandoff::LoginNamesAnother),
    ] {
        let mut progress = refused(why, decided);
        for age in [
            Duration::ZERO,
            NO_WALL_STANDS,
            NO_WALL_STANDS + Duration::from_secs(1),
            Duration::from_secs(7 * 24 * 3600),
        ] {
            assert_eq!(
                answer(&mut progress, &intent, decided + age),
                (NOT_NEEDED.to_owned(), false),
                "{why} at {age:?}"
            );
            assert!(
                matches!(
                    progress,
                    Progress::NotNeeded(NoneComing { end: kind, decided: at, .. })
                        if kind == end && at == decided
                ),
                "{why} at {age:?}"
            );
        }
    }
}

/// AN ASK THAT STARTS A READ STARTS IT, ONCE AND UNLOCKED: a new job's first ask, and the first
/// ask past a no-wall answer's standing time, each run the read exactly once and are answered
/// `waiting`, with the progress already `Reading` and its lock free for the read to write its
/// result or its failure to start. No other ask runs it: not one while that read is out, and
/// not one an answer still stands for.
#[test]
fn an_ask_that_starts_a_read_runs_it_once_with_the_progress_unlocked() {
    let decided = Instant::now();
    let intent = intent();
    let expired = decided + NO_WALL_STANDS;
    for (first, at) in [
        (Progress::Retry(decided), decided),
        (refused("not-walled", decided), expired),
    ] {
        let progress = Mutex::new(first);
        let reads = std::cell::Cell::new(0_u32);
        let read = || {
            reads.set(reads.get() + 1);
            assert!(
                matches!(progress.try_lock().as_deref(), Ok(Progress::Reading(_))),
                "the read runs with the progress unlocked and already reading"
            );
        };
        assert_eq!(ask(&progress, &intent, at, read), WAITING);
        assert_eq!(reads.get(), 1, "the ask that starts a read runs it");
        assert_eq!(
            ask(&progress, &intent, at + Duration::from_secs(5), read),
            WAITING
        );
        assert_eq!(reads.get(), 1, "one read at a time");
    }
    let standing = Mutex::new(refused("not-walled", decided));
    assert_eq!(
        ask(&standing, &intent, decided + Duration::from_secs(5), || {
            panic!("an answer that stands reads nothing")
        }),
        NOT_NEEDED
    );
}
