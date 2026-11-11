// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0

use super::*;

fn scratch(name: &str) -> (aterm_tempfile::TempDir, std::path::PathBuf) {
    let d = crate::test_scratch_dir(&format!("pkg-{name}"));
    let status = d.path().join("status.toml");
    (d, status)
}

#[test]
fn the_stamps_are_read_off_the_record() {
    let (_dir, p) = scratch("stamps");
    assert_eq!(Stamps::read(&p), Stamps::default(), "absent file");
    std::fs::write(
        &p,
        "schema = 1\nupdated_at = \"2026-09-10T09:00:00Z\"\n\
         last_success_at = \"2026-09-10T06:40:53Z\"\nlast_pass = \"failed\"\n\
         last_pass_at = \"2026-09-10T08:34:12Z\"\nlast_index_build = 43\n",
    )
    .unwrap();
    assert_eq!(
        Stamps::read(&p),
        Stamps {
            last_success: rfc3339_to_unix("2026-09-10T06:40:53Z"),
            last_attempt: rfc3339_to_unix("2026-09-10T08:34:12Z"),
            last_outcome: Some(PassOutcome::Failed),
            last_index_build: 43,
            last_pass_target: None,
            in_flight: false,
        }
    );
    std::fs::write(
        &p,
        "updated_at = \"2026-09-10T09:00:00Z\"\nlast_success_at = \"2026-09-10T08:34:12Z\"\n",
    )
    .unwrap();
    let only = Stamps::read(&p);
    assert_eq!(only.last_attempt, only.last_success);
    assert_eq!(only.last_outcome, None);
    assert!(!only.last_failed());
    assert_eq!(only.last_index_build, 0);
    std::fs::write(
        &p,
        "last_success_at = \"2026-09-10T08:34:12Z\"\nlast_pass = \"failed\"\n\
         last_pass_at = \"2026-09-10T07:00:00Z\"\n",
    )
    .unwrap();
    let healed = Stamps::read(&p);
    assert_eq!(healed.last_attempt, healed.last_success);
    assert!(!healed.last_failed(), "a success since the failed pass");
    std::fs::write(
        &p,
        "last_pass = \"settled\"\nlast_pass_at = \"2026-09-10T07:00:00Z\"\n",
    )
    .unwrap();
    assert_eq!(Stamps::read(&p).last_outcome, Some(PassOutcome::Failed));
    std::fs::write(&p, "last_pass = \"ok\"\nlast_pass_at = \"\"\n").unwrap();
    assert_eq!(Stamps::read(&p).last_outcome, None);
    std::fs::write(&p, "not = [valid toml").unwrap();
    assert_eq!(Stamps::read(&p), Stamps::default());
}

#[test]
fn a_pass_records_its_outcome_by_what_it_stamped() {
    assert_eq!(PassOutcome::of_pass(true, 0), PassOutcome::Ok);
    assert_eq!(PassOutcome::of_pass(true, 1), PassOutcome::Ok);
    assert_eq!(
        PassOutcome::of_pass(false, PASS_OFFLINE_EXIT),
        PassOutcome::Offline
    );
    for code in [0, 1, 2, 75] {
        assert_eq!(
            PassOutcome::of_pass(false, code),
            PassOutcome::Failed,
            "{code}"
        );
    }
    for outcome in [PassOutcome::Ok, PassOutcome::Offline, PassOutcome::Failed] {
        assert_eq!(PassOutcome::from_word(outcome.word()), outcome);
    }
}

#[test]
fn a_write_after_the_last_success_is_no_failed_pass() {
    let now = 1_790_000_000_i64;
    let hours = |n: i64| n * 3600;
    let success = now - hours(7);
    let text = |outcome: &str, pass_at: i64| {
        format!(
            "updated_at = \"{}\"\nlast_success_at = \"{}\"\nlast_pass = \"{outcome}\"\n\
             last_pass_at = \"{}\"\n",
            aterm_types::rfc3339::format_rfc3339((now - 60).unsigned_abs()),
            aterm_types::rfc3339::format_rfc3339(success.unsigned_abs()),
            aterm_types::rfc3339::format_rfc3339(pass_at.unsigned_abs()),
        )
    };
    let ok = Stamps::parse(&text("ok", success));
    assert!(!ok.last_failed());
    assert_eq!(full_pass_owed(&ok, now), Some(Owed::Interval));
    let failed = Stamps::parse(&text("failed", now - hours(1)));
    assert!(failed.last_failed());
    assert_eq!(full_pass_owed(&failed, now), None);
    assert_eq!(
        full_pass_due_in(&failed, now),
        FULL_PASS_INTERVAL_SECS - 3600,
        "six hours after the failed pass, not after the write"
    );
}

#[test]
fn a_failure_is_healed_only_by_a_later_ok_pass_and_a_launch_retries_it_at_once() {
    let now = 1_790_000_000_i64;
    let at = |unix: i64| aterm_types::rfc3339::format_rfc3339(unix.unsigned_abs());
    let record = |success: Option<i64>, outcome: &str, pass_at: i64| {
        Stamps::parse(&format!(
            "{}last_pass = \"{outcome}\"\nlast_pass_at = \"{}\"\n",
            success.map_or(String::new(), |s| format!(
                "last_success_at = \"{}\"\n",
                at(s)
            )),
            at(pass_at),
        ))
    };
    let own_failure = now - 600;
    assert!(!healed(&Stamps::default(), own_failure));
    let cached = record(Some(now - 7200), "failed", now - 60);
    assert!(
        !healed(&cached, own_failure),
        "a cache-served pass reached nothing"
    );
    let reached = record(Some(now - 60), "ok", now - 60);
    assert!(healed(&reached, own_failure));
    assert!(
        !healed(&record(Some(own_failure), "ok", own_failure), own_failure),
        "same second: not after"
    );
    let interval = i64::try_from(FULL_PASS_INTERVAL_SECS).unwrap();
    assert_eq!(launch_due_in(&Stamps::default(), now), 0, "never checked");
    assert_eq!(launch_due_in(&cached, now), 0, "the last pass failed");
    assert_eq!(
        launch_due_in(&record(Some(now - 60), "ok", now - 60), now),
        FULL_PASS_INTERVAL_SECS - 60
    );
    assert_eq!(
        launch_due_in(&record(Some(now - interval), "ok", now - interval), now),
        0
    );
}

#[test]
fn a_pass_with_nothing_to_check_counts_as_the_last_success() {
    let now = 1_790_000_000_i64;
    let at = |unix: i64| aterm_types::rfc3339::format_rfc3339(unix.unsigned_abs());
    let empty = Stamps::parse(&format!(
        "last_pass = \"ok\"\nlast_pass_at = \"{}\"\n",
        at(now - 600)
    ));
    assert_eq!(empty.last_success, Some(now - 600));
    assert!(!empty.last_failed());
    assert_eq!(full_pass_owed(&empty, now), None);
    assert_eq!(
        full_pass_owed(
            &empty,
            now - 600 + i64::try_from(FULL_PASS_INTERVAL_SECS).unwrap()
        ),
        Some(Owed::Interval)
    );
    let later = Stamps::parse(&format!(
        "last_success_at = \"{}\"\nlast_pass = \"ok\"\nlast_pass_at = \"{}\"\n",
        at(now - 60),
        at(now - 600)
    ));
    assert_eq!(later.last_success, Some(now - 60));
    let failed = Stamps::parse(&format!(
        "last_pass = \"failed\"\nlast_pass_at = \"{}\"\n",
        at(now - 600)
    ));
    assert_eq!(failed.last_success, None, "the negative control");
    assert!(failed.last_failed());
}

#[test]
fn a_full_pass_is_owed_on_the_interval_or_never_checked_and_never_back_to_back() {
    let now = 1_790_000_000_i64;
    let interval = i64::try_from(FULL_PASS_INTERVAL_SECS).unwrap();
    let spacing = i64::try_from(PASS_SPACING_SECS).unwrap();
    let at = |success: Option<i64>, attempt: Option<i64>| Stamps {
        last_success: success,
        last_attempt: attempt,
        ..Stamps::default()
    };
    assert_eq!(
        full_pass_owed(&Stamps::default(), now),
        Some(Owed::NeverChecked),
        "no record at all"
    );
    let installing = Stamps {
        in_flight: true,
        ..Stamps::default()
    };
    assert_eq!(
        full_pass_owed(&installing, now),
        None,
        "a pass is installing now: the next look sees what it left"
    );
    assert_eq!(full_pass_due_in(&installing, now), PASS_SPACING_SECS);
    let checked = now - interval + 60;
    assert_eq!(full_pass_owed(&at(Some(checked), Some(checked)), now), None);
    assert_eq!(
        full_pass_due_in(&at(Some(checked), Some(checked)), now),
        60,
        "the rest of the interval"
    );
    assert_eq!(
        full_pass_owed(&at(Some(now - interval), Some(now - interval)), now),
        Some(Owed::Interval),
        "six hours to the second"
    );
    assert_eq!(
        full_pass_owed(&at(Some(now - 2 * interval), Some(now - 60)), now),
        None,
        "owed, but a sibling attempted a minute ago"
    );
    assert_eq!(
        full_pass_owed(&at(Some(now + 3600), Some(now - spacing)), now),
        None
    );
    assert_eq!(
        full_pass_due_in(&at(Some(now + 3600), Some(now - spacing)), now),
        FULL_PASS_INTERVAL_SECS,
        "never longer than one interval"
    );
    assert_eq!(
        full_pass_owed(&at(Some(now + 2 * interval), Some(now - spacing)), now),
        Some(Owed::Interval)
    );
    assert!(!at(None, Some(now + 2 * spacing)).attempted_recently(now));
    assert!(at(None, Some(now + spacing - 1)).attempted_recently(now));
}

#[test]
fn a_failing_pass_is_owed_again_only_after_the_interval() {
    let now = 1_790_000_000_i64;
    let interval = i64::try_from(FULL_PASS_INTERVAL_SECS).unwrap();
    let spacing = i64::try_from(PASS_SPACING_SECS).unwrap();
    let at = |success: Option<i64>, attempt: Option<i64>| Stamps {
        last_success: success,
        last_attempt: attempt,
        last_outcome: attempt.map(|attempt| {
            if success == Some(attempt) {
                PassOutcome::Ok
            } else {
                PassOutcome::Failed
            }
        }),
        ..Stamps::default()
    };
    for never in [
        at(None, Some(now - spacing)),
        at(None, Some(now - 2 * 3600)),
        at(None, Some(now - interval + 1)),
    ] {
        assert!(never.last_failed());
        assert_eq!(full_pass_owed(&never, now), None, "{never:?}");
    }
    assert_eq!(
        full_pass_due_in(&at(None, Some(now - spacing)), now),
        FULL_PASS_INTERVAL_SECS - PASS_SPACING_SECS
    );
    assert_eq!(
        full_pass_owed(&at(None, Some(now - interval)), now),
        Some(Owed::NeverChecked),
        "an interval on: owed again"
    );
    let walk_failed = at(Some(now - 4 * interval), Some(now - 3600));
    assert!(walk_failed.last_failed());
    assert_eq!(full_pass_owed(&walk_failed, now), None);
    assert_eq!(
        full_pass_owed(&walk_failed, now + interval - 3600),
        Some(Owed::Interval)
    );
    let walk_ok = at(Some(now - interval), Some(now - interval));
    assert!(!walk_ok.last_failed());
    assert_eq!(full_pass_owed(&walk_ok, now), Some(Owed::Interval));
    assert!(!Stamps::default().last_failed(), "never attempted");
}

#[test]
fn a_lane_slot_is_claimed_once_per_period() {
    let (dir, _) = scratch("claim");
    let stamp = dir.path().join("machine-apply.stamp");
    let now = 1_790_000_000_i64;
    let day: u64 = 24 * 60 * 60;
    let day_secs = i64::try_from(day).unwrap();
    assert!(claim(&stamp, now, day), "first claim");
    assert_eq!(
        std::fs::read_to_string(&stamp).unwrap().trim(),
        now.to_string()
    );
    assert!(!claim(&stamp, now + 60, day), "inside the day");
    assert!(!claim(&stamp, now + day_secs - 1, day));
    assert!(claim(&stamp, now + day_secs, day), "a day later");
    assert!(!claim(&stamp, now + day_secs + 1, day), "and stamped again");
    std::fs::write(&stamp, "garbage").unwrap();
    assert!(claim(&stamp, now, day), "an unreadable stamp is claimable");
    std::fs::write(&stamp, format!("{}\n", now + 3 * day_secs)).unwrap();
    assert!(claim(&stamp, now, day), "a far-future stamp is claimable");
    let nowhere = dir.path().join("absent/dir/x.stamp");
    assert!(
        claim(&nowhere, now, day),
        "no directory: nothing to hold it"
    );
    assert!(claim(&nowhere, now, day), "…and nothing refuses either");
    #[cfg(unix)]
    {
        let target = dir.path().join("symlink-target.txt");
        std::fs::write(&target, format!("{now}\n")).unwrap();
        let linked = dir.path().join("linked.stamp");
        std::os::unix::fs::symlink(&target, &linked).unwrap();
        assert!(
            claim(&linked, now + 60, day),
            "a symlinked stamp is ignored rather than followed"
        );
        assert_eq!(
            std::fs::read_to_string(&target).unwrap(),
            format!("{now}\n"),
            "symlink target must never be overwritten by claim"
        );
    }
}

#[test]
fn a_claim_held_elsewhere_is_not_taken_twice() {
    let (dir, _) = scratch("claim-held");
    let stamp = dir.path().join("session-pass.stamp");
    let held = crate::FileLock::acquire(&stamp.with_extension("lock")).unwrap();
    let started = std::time::Instant::now();
    assert!(!claim(&stamp, 1_790_000_000, PASS_SPACING_SECS));
    assert!(
        started.elapsed() < std::time::Duration::from_secs(60),
        "the claim blocked past its wait"
    );
    drop(held);
    assert!(claim(&stamp, 1_790_000_000, PASS_SPACING_SECS));
}

#[test]
fn the_stamp_parser_is_pinned() {
    assert_eq!(rfc3339_to_unix("1970-01-02T00:00:00Z"), Some(86_400));
    assert_eq!(
        rfc3339_to_unix("2026-09-10T06:40:53.123Z"),
        rfc3339_to_unix("2026-09-10T06:40:53Z")
    );
    assert_eq!(
        rfc3339_to_unix("2026-09-10T06:40:53.000000000Z"),
        rfc3339_to_unix("2026-09-10T06:40:53Z"),
        "the pass writer's own shape"
    );
    for bad in [
        "",
        "2026-09-10",
        "2026/09/10T06:40:53Z",
        "2026-13-10T06:40:53Z",
        "2026-09-10T06:40:53+05:30",
        "2026-09-10T06:40:53.123-08:00",
        "2026-09-10T06:40:53",
        "2026-09-10T06:40:53Zjunk",
    ] {
        assert_eq!(rfc3339_to_unix(bad), None, "{bad:?}");
    }
}

#[test]
fn completed_older_index_pass_requires_settled_matching_attempt_below_published() {
    let stamps = Stamps::parse(
        "last_pass_at = \"2026-09-10T06:40:53.000000000Z\"\n\
         last_pass = \"ok\"\n\
         last_pass_attempted_at = \"2026-09-10T06:40:53.000000000Z\"\n\
         last_pass_attempted_index_build = 100\n",
    );
    assert!(stamps.completed_older_index_pass(101));
    assert!(!stamps.completed_older_index_pass(100));
    assert!(!stamps.completed_older_index_pass(99));

    let mut in_flight = stamps;
    in_flight.in_flight = true;
    assert!(!in_flight.completed_older_index_pass(101));

    let mut no_outcome = stamps;
    no_outcome.last_outcome = None;
    assert!(!no_outcome.completed_older_index_pass(101));

    let mut superseded_attempt = stamps;
    superseded_attempt.last_attempt = stamps.last_attempt.map(|t| t + 10);
    assert!(!superseded_attempt.completed_older_index_pass(101));
}
